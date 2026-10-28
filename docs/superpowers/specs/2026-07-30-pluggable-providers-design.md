# Design Spec: Pluggable Edge Cookie, Device, and Geo Modules

**Status:** Proposed. Raised as pull requests
IABTechLab/trusted-server#1343 (row 2 of the stack, the Edge Cookie module
seam) and IABTechLab/trusted-server#1344 (row 3, device and geo selection),
neither of which is merged to main. Both were reviewed earlier, as pull
requests #1043 and #1044, which are closed.
**Author:** Engineering
**Issue references:** #777, #778, #780, #781
**Related specs:** `2026-07-30-permission-model-design.md`,
`2026-07-30-provider-migration-rollout-design.md`,
`2026-07-30-client-cycle-ec-resolve-design.md`
**Last updated:** 2026-10-16

> **Why this spec exists.** Core once decided three things for every
> deployment, which are whether to create an Edge Cookie identity, how to
> classify the requesting device and whether to resolve a location. Each is now
> a module that an operator selects. This spec states what a module may do,
> what core still decides for every module, and how a selection that cannot be
> satisfied fails. The spec avoids four faults an identity abstraction can
> have, which are owning creation without owning recognition, differing between
> adapters, accepting a misconfiguration silently, and carrying trait methods
> that nothing calls.

**Scope.** This page describes what the code on the branch `split/2-device-geo`
does. That branch holds rows 2 and 3 of the stack that
IABTechLab/trusted-server#777 lists, and IABTechLab/trusted-server#1342 holds
every design document of the stack. Where the page describes something that a
later row builds, the text names the row and says that this branch does not have
it. The rows the page names are:

| Row | Branch                          | Pull request                   | What the row builds                            |
| --- | ------------------------------- | ------------------------------ | ---------------------------------------------- |
| 2   | `split/1-ec-provider`           | IABTechLab/trusted-server#1343 | The Edge Cookie module seam (on this branch)   |
| 3   | `split/2-device-geo`            | IABTechLab/trusted-server#1344 | Device and geo selection (on this branch)      |
| 4   | `split/3-permissions`           | IABTechLab/trusted-server#1345 | The permission model                           |
| 5   | `split/4-client-resolve`        | IABTechLab/trusted-server#1346 | The client-set Edge Cookie value path          |
| 7   | `split/7-integration-seam-impl` | IABTechLab/trusted-server#1348 | The integration seam, with module registration |

---

## 1. Overview and goals

Trusted Server makes three per-request data decisions that were previously
hard-wired: whether to create or keep an Edge Cookie (EC) identity, how to
classify the requesting device, and whether to resolve geolocation. Each is now
a **module**, a selectable component chosen in operator configuration.

Goals, as the code on this branch meets them:

- A deployment picks an Edge Cookie module, or none, without a change to core.
  Core builds `hmac` and `host_signals` itself and takes any other module from
  the adapter that injects it. Device and geo are narrower. Core names the
  selections it accepts (`builtin` and `fastly` for device, `platform` and
  `none` for geo), so a third device or geo implementation needs a change in
  core until row 7 lets a registration supply one.
- The Edge Cookie and device defaults are neutral. With no configuration, no EC
  is created and device classification uses only the User-Agent. The geo
  default is not neutral. The host platform's lookup runs unless
  `[geo] module = "none"` turns it off, so a default deployment on Fastly calls
  Fastly's geolocation. The Fastly entry point also reads the TLS and HTTP/2
  signals on every request, whichever device module is selected (section 5).
- Core gates creation, and a module cannot authorize itself. An identifier is
  created only when `ec_consent_granted` accepts the request's jurisdiction and
  consent signals (section 5). Row 4 replaces that check with a declaration that
  the module makes (`required_permissions`) and core enforces.
- An Edge Cookie selection that an adapter cannot satisfy fails loudly when the
  adapter starts (section 6), and all four adapters (Fastly, Axum, Cloudflare,
  Spin) route the geo selection through the same core builder. Parity stops
  there. Only the Fastly adapter builds the device module, creates identifiers
  and registers the EC API routes (identify and batch-sync), because the
  portability adapters do not yet wire a platform KV store. On Axum, Cloudflare
  and Spin, `[device] module = "fastly"` passes validation and nothing reads it.
  The Spin adapter's route list records the missing routes rather than leaving
  those paths silent. Section 7 has the table of what each adapter wires.

Non-goals:

- No vendor module ships in this epic beyond the host-platform implementations
  named below. The `crates/edgecookie/` directory holds a README describing
  where vendor EC crates will live.
- The client-cycle (browser round-trip) module type has its own spec. Row 5
  builds its seam (`resolve_from_client`, a no-op by default) and a
  demonstration module (`client_fixed`) compiled only into test and
  demonstration builds, whose selection in a production build is a startup
  error. This branch has none of these.

## 2. Module taxonomy

### 2.1 The configuration convention every module type follows

Every pluggable thing in Trusted Server is a **module**, meaning a component a
deployment selects by name in operator configuration rather than one core
hard-wires. Each module type is one top-level table, and a selector key inside
that table selects what runs. The key is `module` where one module runs and
`modules` where several run:

```toml
[<type>]
module = "<name>"            # modules = ["<name>", ...] where several run

[<type>.<name>]              # only when that module has settings
setting = "value"
```

The seven types, all singular, with the row that builds each:

| Type                | What it decides                          | Selector  | It takes                   | Row |
| ------------------- | ---------------------------------------- | --------- | -------------------------- | --- |
| `ec`                | Edge Cookie identity                     | `module`  | a string, one runs         | 2   |
| `geo`               | location resolution                      | `module`  | a string, one runs         | 3   |
| `device`            | device classification                    | `module`  | a string, one runs         | 3   |
| `permission-signal` | which permission signal schemes run      | `modules` | a list, in the order given | 4   |
| `demand`            | the sources bids are requested from      | `modules` | a list, all run            | 7   |
| `ad-server`         | the ad server that decides what is shown | `module`  | a string, one runs         | 7   |
| `integration`       | the vendor modules a page loads          | `modules` | a list, all run            | 7   |

Row 7 selects a page integration in the section of its own type (for example
`[cmp]`, `[auction]` or `[proxy]`) and refuses a section named `[integration]`,
so the `integration` type is a family of sections and not a table of its own.

The rules the stack settles on, which hold for all seven types once every row is
merged:

- A `[<type>.<name>]` table exists only where that module has something to set.
  A module with no settings is selected by name alone.
- The word `providers` appears nowhere in configuration. The type table, and the
  `module` or `modules` key inside it, carry the whole selection.
- The name is the implementation, unless the table carries an `implementation`
  line, which names the implementation by its module path. A demand table always
  carries one, because `demand` is not the type its implementations are named
  under, and it is how two Prebid Servers run side by side under different
  names, each with its own endpoint and its own settings.
- A module from a crate is named by its folder below `crates/`, with `.` between
  the parts, and may be written without its own section's type folder. A
  `demand` or `ad-server` name is snake_case, because it may be a label of the
  operator's own.
- A `[<type>.<name>]` table that its type's selector does not select refuses
  startup, because an unreferenced table is a mistyped selector or a stale
  block, and accepting one silently invites configuration drift.
- An implementation this build does not have refuses startup, and the message
  lists the implementations it does have.
- A `demand` or `ad-server` endpoint must be HTTPS, or HTTP to a loopback host
  only (`127.0.0.1`, `::1`, `localhost`).
- Every module rejects settings it does not know, so a typo fails loudly rather
  than being quietly ignored.
- A secret setting holds the **name** of a key in `trusted_server_secrets`,
  never a value, so no secret is ever written into a configuration file.

What this branch has of those rules:

- Three of the seven types exist, which are `ec`, `geo` and `device`. Row 4
  (IABTechLab/trusted-server#1345) adds `permission-signal`, and row 7
  (IABTechLab/trusted-server#1348) adds `demand` and `ad-server`. Until row 7
  the auction keeps its `[auction.providers.<name>]` and
  `[auction.bidders.<name>]` maps, so the word `providers` still appears in
  configuration. `[ec.providers]` is refused by name, because the Edge Cookie
  settings moved out of it.
- Only `ec` has module tables, the `implementation` line and names taken from a
  crate's folder. `[device]` and `[geo]` hold the `module` key alone, and any
  other key under them is refused.
- An implementation the build does not have refuses startup for all three types.
  The message lists what the build has for `ec` alone, and the Edge Cookie
  refusal comes when the adapter builds its application state and not when the
  settings are read (section 6).
- The sections and the built-in module tables refuse keys they do not know. A
  module that an adapter injects reads its own table, so core cannot apply the
  rule to it.
- The `hmac` and `host_signals` passphrase is the one secret setting. It holds
  the name of a `trusted_server_secrets` key.

Row 7 also changes `[auction]`. From that row `[auction]` is not a module type of
its own, and `[auction]` keeps `enabled`, `timeout_ms`, the creative settings,
`allowed_context_keys`, and `[auction.bidders.<code>] module = "<demand name>"`,
which maps a bidder code a page asks for onto one of the declared demand
modules. The `modules` list in `[auction]` selects the page modules the auction
runs, such as `prebid` (seam design §2).

### 2.2 The three module types this spec defines

| Concern     | Type     | Trait              | Default                            | Other selections                                                                                                                              |
| ----------- | -------- | ------------------ | ---------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| EC identity | `ec`     | `EdgeCookieModule` | none (stateless)                   | `hmac` (HMAC over the client IP, keeps today's identifier hash), `host_signals` (see below), and from row 5 `client_fixed` (demo builds only) |
| Device      | `device` | `DeviceModule`     | `builtin` (User-Agent only)        | `fastly` (TLS JA4 and HTTP/2 signals through an injected `HostSignals` service)                                                               |
| Geo         | `geo`    | `PlatformGeo`      | `platform` (the host's own lookup) | `none` (no location, no host call)                                                                                                            |

The geo trait is the existing `PlatformGeo` in `platform/traits.rs` rather than
a new `GeoModule` name. The EC trait lives in `ec/module.rs` and the device
trait in `ec/device.rs`.

Core names the implementations it builds. It names `hmac` and `host_signals`
(`HMAC_MODULE_KEY` and `HOST_SIGNALS_MODULE_KEY` in `ec/module.rs`), `builtin`
and `fastly` for device (`ec/device.rs` and `DeviceConfig` in `settings.rs`), and
`platform` and `none` for geo (`build_geo_module` in `platform/mod.rs` and
`GeoConfig` in `settings.rs`). The host-specific pieces come from the adapter.
The Fastly adapter supplies `FastlyDeviceModule` and `FastlyHostSignals`, and
each adapter passes its own host geo lookup to `build_geo_module`.

The integration registration builder has no hook for an Edge Cookie, device or
geo module. `IntegrationRegistrationBuilder` in `integrations/registry.rs` offers
proxies, rewriters, head injectors and request filters, so none of the three can
be registered the way a vendor page integration is. A vendor Edge Cookie module
reaches `build_module` as its `injected` argument. The Fastly adapter passes
none, and Axum, Cloudflare and Spin pass only a module that a caller put into
the `RuntimeServices` it supplied. A vendor device or geo module needs a change
in core. Row 7 adds `with_ec_module`, `with_device_module` and `with_geo_module`
to the registration builder, so a crate can supply any of the three and ship no
browser JavaScript at all. The integration seam spec states that rule in full.

Selection keys are strings in operator configuration
(`trusted-server.example.toml` carries the commented template):

```toml
[ec]
module = "hmac"

[ec.hmac]
passphrase = "ec_passphrase"   # names a trusted_server_secrets key

[device]
module = "builtin"   # default. "fastly" opts into TLS/H2 signal evidence

[geo]
module = "platform"  # default. "none" resolves no location and makes no host call
```

`passphrase` holds the name of a key in `trusted_server_secrets`, never the
secret. The secret itself must be at least 32 bytes and must not be one of the
known placeholder values. The `ts` CLI can replace a selector from a
`TRUSTED_SERVER__` environment variable when it builds the configuration for
push (`TRUSTED_SERVER__EC__MODULE` for `[ec] module`). The overlay replaces only
a scalar that is already in the TOML file, so a commented-out selector is not
changed, and the running server reads the pushed configuration and never the
environment.

**The `host_signals` EC module** creates an identifier from an HMAC over the
host's TLS JA4 and HTTP/2 signals plus the client IP, keyed by the configured
passphrase. Row 3 builds it as an opt-in implementation (`[ec.host_signals]`)
against the host-agnostic `HostSignals` capability and not a Fastly API, so any
host that supplies the signals can run it and a host that supplies none cannot
build it. Only the Fastly adapter supplies them today, so Axum, Cloudflare and
Spin refuse the selection when they build application state. When a request
carries neither signal the module defers with a warning, meaning it creates no
identity for that request, instead of degrading to an identifier made from the
IP alone under the `host_signals` name. The module's identifiers carry the code
`hs00` and the same `<64 hex>.<6 alphanumeric>` shape as the built-in HMAC
module's. **Whether this module ships in the series at all is an open review
question.** The migration rollout spec carries the question as sign-off row 22,
which proposes deferring host signal processing to a separate design, and this
spec does not present the module as settled either way.

## 3. The identity lifecycle contract

An identity abstraction that owned **creating** an identifier but left
**recognition** and **KV key normalization** hard-coded to the built-in HMAC
shape would fail for any module whose identifiers did not match
`{64hex}.{6alnum}`, because the cookie it created would be discarded by the very
next request. So every lifecycle operation core performs on an EC value goes
through the selected module:

| Lifecycle operation | Where core uses it                                                                                                                                                                                                                                                | Contract                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| ------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Create**          | EC generation on a document navigation, which only the Fastly adapter does (section 7). The client-cycle resolve endpoint creates from row 5                                                                                                                      | The module returns the identifier (`generate`) and only core writes the cookie, after enforcing the global bounds below. Row 5 adds `resolve_from_client` for the client cycle.                                                                                                                                                                                                                                                                                                                                                                                                                         |
| **Recognize**       | Reading `ts-ec` back from the request, deciding `ec_was_present`, withdrawal checks, and every path that hands the value onward: the origin URL in `append_ec_id`, the click-target URL in `handle_first_party_click`, and the proxied body an integration builds | `accepts_id` answers whether a value is a well-formed identifier the module issues. A value the selected module does not recognize is treated as absent, so it is never used or sent onward, while the raw cookie value stays visible to withdrawal handling. The paths that send a value onward reach the same answer through `edge_cookie::recognized_ec_id`, and a deployment with no module selected recognizes nothing and so sends nothing onward.                                                                                                                                                |
| **KV key**          | Identity-graph row reads and writes                                                                                                                                                                                                                               | Core builds the key as the identifier's `{code}~` prefix followed by `normalize_id_for_kv` of the value part (`module_kv_key`). A new `hmac` identifier is therefore keyed `hmac~<hash>.<suffix>`. A bare pre-envelope identifier has no prefix and is keyed `<hash>.<suffix>`, which keeps every row written before these rows reachable. The default lowercases the built-in HMAC hash segment and preserves the suffix. An opaque or case-sensitive module overrides it to the identity function so distinct identifiers never collapse into one row.                                                |
| **Withdraw**        | Expiring the cookie and writing revocation markers                                                                                                                                                                                                                | Withdrawal runs when the consent model finds an explicit withdrawal signal (`ec_consent_withdrawn`) and the browser sent a `ts-ec` cookie. The identifiers eligible for a **graph tombstone**, meaning the row is kept and marked as withdrawn, are exactly those the selected module owns, dispatched on the `{code}~` prefix first and then `accepts_id`, never a shape check the module cannot influence. Expiring the **cookie** is broader, because it keys off the raw cookie being present, so it still fires for an identifier the selected module does not own (see the switching case, §6.1). |

**Invariant:** for every module `P` and every identifier `id` created by `P`, `id`
round-trips read-back byte for byte. A test in `ec/mod.rs`
(`read_from_request_round_trips_an_opaque_module_identifier`) proves the
round-trip with a non-default module whose identifiers are opaque, meaning core
does not interpret their format. Row 5 adds a test in `ec/resolve.rs` that
proves the client-cycle value survives the full scenario verbatim.

The IP-cluster prefix listing runs unchanged, but the key space it lists over
does not. A fresh create is keyed `hmac~<hash>.<suffix>`, so the prefix that
`evaluate_cluster` derives (everything before the first `.`) is `hmac~<hash>`
for a coded row, while a bare row still lists under `<hash>` on its own. Prefix
matching is anchored at the start of the key, so two rows for the same client IP
that straddle the envelope never count each other, and `cluster_size`
under-reports for as long as both kinds of row exist.

That undercount is accepted rather than bridged, for two reasons. First,
`cluster_size` is returned in the identify response (`ec/identify.rs`) and gates
nothing, because no code compares it with any value, and the settings a reader
might expect to gate on it, `cluster_trust_threshold` and `cluster_recheck_secs`,
have no readers outside the settings definition. Second, bridging the undercount
would mean a second prefix scan on every identify request for as long as both
kinds of row exist. The undercount does not end by itself. The `hmac` module
reads the bare form with no end date (section 4), and nothing rewrites a bare
cookie or row as a coded one, so the undercount lasts while rows of both kinds
exist. It becomes a real fault only if a later change makes the count gate
something, and that is the change that has to build the bridge.

One global rule sits above every module, and it is built:

- **Identifier bounds.** A created identifier obeys a global cookie-safe alphabet
  (normatively `[A-Za-z0-9._~-]`, valid cookie octets with no separators,
  whitespace, or control characters) and a global maximum of **256 bytes**,
  stated here so dependent documents reference one number. The bound applies to
  the identifier itself, not only its key form. Core enforces the bound wherever
  an identifier enters the system, at create (on what `generate` returns, and
  from row 5 on what the resolve endpoint receives), at cookie read-back, and at
  cookie write. The constant is `MAX_EC_ID_LEN` in `ec/cookies.rs`. A violating
  value is rejected outright and logged. No sanitizing rewrite exists anywhere on
  the path, so an identifier survives byte for byte or not at all, and the cookie
  value and the identity-graph key can never silently diverge.

## 4. Trait surface: minimalism rule, and where it does not apply

Every trait method must have at least one production (non-test) caller in the
same change that introduces it. **This rule does not apply to an evidence
interface.**

An evidence interface describes what a request carries, not what today's code
happens to read. Holding an evidence interface to the caller rule produces an
interface that grows a method each time a vendor arrives, which is not something
a vendor can write against and cannot be stable across a release. The rule also
puts the boundary in the wrong place, because what a module may see is not the
control. What a module may do with what it sees is the control, and that is the
permission model.

So `RequestInfo` offers what a request carries that a module could need, whether
or not code in this repository reads it yet. `RequestInfo` has the normalized
client IP, the User-Agent, any request header by name, the names of all headers,
the path, the query string and one query parameter by name (`evidence.rs`). A new
accessor is added as a defaulted method, so an existing implementation keeps
compiling.

The rule stands for behavioral traits, where a method with no caller really is
dead weight. How the surface resolved on this branch:

- `keys_equal`: **not built.** Equivalent-envelope comparison (#778) is served
  structurally, because read-back acceptance and KV normalization both route
  through the module, so no comparison method exists to leave uncalled.
- `GeneratedEdgeCookie::response_headers`: **built, with a production caller.**
  EC finalization applies module-requested headers to the outbound response
  (`ec/finalize.rs`), after core checks each one against its reserved surface
  (`reserved_response_effect`). Row 5 also returns them from the client-cycle
  resolve path, which is how a client-side module requests further evidence from
  the page.
- `IdentityInput.consent`: **built and populated.** The organic create path
  passes the request's consent context so a module can read it for behavior
  beyond gating (`EcContext::candidate_id`). The gate has already run before
  `generate` is called, so a module cannot use the field to authorize itself.
  `IdentityInput.permissions` comes with row 4.
- `required_permissions` on the three traits: **not on this branch.** Row 4
  adds it to `EdgeCookieModule`, `DeviceModule` and `PlatformGeo` with an empty
  default, and core enforces it as a request gate for the EC module only
  (section 5).

The `EdgeCookieModule` surface on this branch (`ec/module.rs`):

```rust
pub trait EdgeCookieModule: Send + Sync + core::fmt::Debug {
    /// Stable implementation id ("hmac"), what `[ec] module` selects by.
    fn id(&self) -> &'static str;
    /// Registered four-character code (provider-code-registry.md), the
    /// `{code}~` namespace of every identifier the module creates.
    /// Mandatory, no default: a module cannot exist without a unique
    /// code, so identifiers from different modules can never collide.
    fn code(&self) -> ModuleCode;
    /// Whether the module was built from evidence about one request, so a
    /// composition root must not keep it for later requests. Default: false.
    fn is_request_scoped(&self) -> bool { false }
    /// Derives an identifier from the request evidence passed at call time.
    /// A module that cannot derive one returns no id and the request
    /// proceeds without an Edge Cookie.
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
}
```

Row 4 adds `required_permissions` to this trait, and row 5 adds
`resolve_from_client` and its `ClientResolveInput`.

Core owns the code envelope. At create it prefixes the module's value with
`{code}~`, at read-back it strips and checks the code before the module's
`accepts_id` sees the value part, and the identity graph key preserves the code
verbatim around the module's canonical form. A cookie carrying another module's
code is treated as absent, never adopted, so switching modules cannot silently
mix identity populations, and a withdrawal always acts on a key that can only
belong to one module. The built-in HMAC module creates
`hmac~<64 hex>.<6 alphanumeric>` and also reads its pre-envelope bare form, so
deployed cookies keep working. The bare form belongs to `hmac` alone, and no
code ends that reading. Codes are allocated append-only in
`provider-code-registry.md`, and a leading digit is valid (`1a2b`).

Request data reaches a module through the `RequestInfo` passed at call time, and
host signals reach a module that needs them through an injected `HostSignals`
service, not through a fixed parameter struct. Core hands `generate` a snapshot
of the request's headers, path and query. Core takes the snapshot only when a
module is selected and either the request carries no recognized identifier or
the request is a document navigation, so a deployment with no module, and a
returning visitor's subresource requests, clone nothing.

## 5. Permission enforcement is core's job, for EC modules

On this branch core gates creation on the consent model, and a module cannot
authorize itself. `EcContext::read_from_request_with_geo` takes the selected
module once and builds the request's consent context from the request's cookies
and headers and from the location the adapter looked up. The function stores the
result as `ec_allowed`, which is true only when a module is selected and
`ec_consent_granted` (`allows_ec_creation` in `consent/mod.rs`) accepts the
consent context. `generate_if_needed` skips creation, with a log line naming the
jurisdiction class (`gdpr`, `us-state`, `non-regulated` or `unknown`), when
`ec_allowed` is false. With no module selected nothing may create or use an
identifier, so the gate is closed rather than open by default.

The gate follows the jurisdiction that the location gives. A GDPR request needs
TCF storage consent. A US state request needs a consent signal and no opt-out,
and Global Privacy Control, a GPP sale opt-out and a US Privacy opt-out each
refuse creation. A non-regulated location is allowed. An unknown jurisdiction,
which is what a request with no location gets, is refused.

Row 4 replaces this gate. The module declares the permissions its data use needs
(`required_permissions` on the trait), core resolves the request's permissions
from the permission model's country and region rules and signals, and generation
is skipped unless the module's declaration is met. The permission model spec
governs that machinery, including the top node of the `permissions.yaml` rules
tree, the `assume_single_jurisdiction` acknowledgment, and `GeoStatus`, which
tells a failed lookup from a lookup that found nothing.

**Recognition and withdrawal always run**, whatever the gate says. Read-back
acceptance (`module_owns_id`) happens before the gate is computed, and
withdrawal handling keeps the raw cookie value even when the identifier is
treated as absent, so an opt-out can always reach the identity it revokes. A
blanket execution gate would refuse to run the module in exactly the state an
opt-out produces.

The gate applies to EC modules **only**. Geo and device are ungated for
different reasons, stated separately because only one of them is structural:

- **Geo: circularity.** The jurisdiction is resolved from the location the geo
  module returns, and the gate reads the jurisdiction. Gating geo on the gate's
  own result is unsatisfiable. `PlatformGeo` has no `required_permissions`
  method on this branch. Row 4 adds one with an empty default for interface
  uniformity, and nothing consults it on the lookup path.
- **Device: using host evidence is opt-in, and capturing it is not.** Device
  classification is not an input to the consent gate. The neutral `builtin`
  classifier reads only the User-Agent and makes no host call, and
  `[device] module = "fastly"` selects the module that adds the TLS JA4 and
  HTTP/2 signals to the browser and bot gate that guards EC creation and
  identity-graph writes. Selecting that module, or `host_signals`, decides only
  whether a module reads the signals. The Fastly entry point reads the signals
  from the SDK on every request, whichever device module is selected, and sets
  them as the internal headers `x-ts-tls-ja4` and `x-ts-h2-fingerprint`,
  replacing any copy the client sent. The adapter then always builds its
  `HostSignals` service from those two headers. Both are listed in
  `HOST_SIGNAL_HEADERS` (`constants.rs`), from which `INTERNAL_HEADERS` is
  built, so the Lockr and Permutive proxies, which copy every `x-` request
  header that list does not name, drop them. `rewrite_origin_request`
  (`publisher.rs`) removes them from the publisher origin request, which
  carries the reader's other headers onward, and every other request core
  sends onward copies a fixed list of headers, so no request leaves with either
  header. The DataDome protection call is separate, because it sends the JA4
  and HTTP/2 values from `ClientInfo` as fields of its own. Identity rows
  persist the derived classification fields (the JA4 class segment and a
  12-hex-character hash prefix of the HTTP/2 SETTINGS signal), not raw signals,
  and the neutral default persists neither because the builtin module produces
  no such fields.
- **Device: a declared permission.** Row 4 gives `DeviceModule` a
  `required_permissions` method with an empty default, and no per-request device
  gate exists to honor a declaration. Row 7 therefore refuses at startup a
  module-supplied device module that declares a nonempty set, so that a
  declaration which reads as a gate is never accepted while nothing enforces it.
  The built-in and host device modules declare the empty set. This branch has
  neither the method nor the refusal.

## 6. Selection, validation, and failure modes

Configuration is checked in two places. `Settings::finalize_deserialized` checks
everything that configuration alone decides, so a mistake it can see is a
startup error and never a silent change in behavior. A selection that only the
running host can satisfy is checked by the adapter while it builds application
state, before the adapter serves a request. That covers an implementation that
core does not build and the adapter does not inject, and a module that needs
host signals the adapter does not supply. Fastly, Cloudflare and Spin keep the module that
check resolved (`build_reusable_module`), and Axum checks and keeps nothing
(`ensure_module_available`). When the adapter's check fails, the adapter serves
an error in place of every route, so a bad selection never degrades into a
deployment that quietly runs stateless. Fastly, Axum and Cloudflare answer with
status 500 and the body "An internal error occurred", and the error text appears
only in the log. The Fastly entry point still answers `GET /health` with 200
before the application is built. Spin answers 503 "Service Unavailable" on every
route except `GET /health`, which answers 200.

| Configuration state                                                                                                           | Behavior                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| ----------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `[ec] module` selects `hmac` or `host_signals`, by name or by an `implementation` line, and its `[ec.<name>]` table is absent | Settings error naming the missing table. Both modules built into core take a passphrase they cannot default, so both need a table. For a module an adapter injects, the adapter reads the table, so core cannot say whether one is needed.                                                                                                                                                                                                           |
| `[ec.<name>]` table present, `module` unset                                                                                   | **Settings error.** A half-migrated configuration would otherwise run stateless and show up as a drop in revenue. An operator who genuinely wants stateless deletes the table.                                                                                                                                                                                                                                                                       |
| `module = "none"` (explicit stateless)                                                                                        | Valid, and means exactly what omitting the selector means. Any configured module table alongside it is a settings error, the same stray-table rule as below.                                                                                                                                                                                                                                                                                         |
| A configured `[ec.<name>]` table that `module` does not select                                                                | **Settings error**, the rule §2.1 states for every module type. An unreferenced table is almost always a mistyped selector or a stale block, and accepting one silently invites configuration drift.                                                                                                                                                                                                                                                 |
| `[ec] module` names an implementation that is neither built into core nor injected by the adapter                             | Error when the adapter builds application state, listing the implementations this deployment has, which are `hmac`, `host_signals` and the injected module's id when there is one. The list appears only in the log, because the response carries the generic body above, and from it the operator can tell whether a module is missing or the name is mistyped.                                                                                     |
| `module = "host_signals"` on an adapter that supplies no host signals                                                         | A startup error of the same kind on Axum, Cloudflare and Spin. On Fastly the adapter always supplies the signals service, and the module is built for each request from that request's signals.                                                                                                                                                                                                                                                      |
| `module = "client_fixed"`                                                                                                     | This branch has no such module, so the row for an unknown implementation applies. Row 5 adds it behind the `client-fixed-demo` cargo feature and makes its selection in a production build a startup error.                                                                                                                                                                                                                                          |
| No `module`, no module table                                                                                                  | Valid, the neutral default for that concern.                                                                                                                                                                                                                                                                                                                                                                                                         |
| Deprecated `[ec] passphrase`                                                                                                  | Migrated to `module = "hmac"` with the passphrase in `[ec.hmac]`, and a deprecation warning names the new location. The deprecated value is held to the same passphrase rules as the new one. Both forms together are rejected so a half-edited file fails loudly instead of one form silently winning.                                                                                                                                              |
| Any unknown key in `[ec]`, `[device]`, `[geo]`, or a module's own table                                                       | Settings error. In `[device]`, `[geo]`, `[ec.hmac]` and `[ec.host_signals]` an unknown key is refused by name. In `[ec]`, a key that is not a table is refused as an unknown field and the message lists the keys `[ec]` reads, and a table is read as a module table, so the stray-table rule applies to it. `[ec.providers]` is refused by name. A key from a feature no row builds, such as `legacy_providers` or `versions`, fails the same way. |
| `[device] module` names an implementation the build does not have                                                             | Settings error saying the module is not available in this build. Unlike the Edge Cookie error, the message does not list what the build has, which is `builtin` (the default) and `fastly`.                                                                                                                                                                                                                                                          |
| `[geo] module` names anything other than `platform` or `none`                                                                 | Settings error saying the module is not available in this build. Valid states are unset (the default, the host platform's lookup), `platform` (the same, spelled out) and `none` (no location).                                                                                                                                                                                                                                                      |
| `[device] module = "fastly"` on Axum, Cloudflare or Spin                                                                      | Accepted, and nothing reads it. Only the Fastly adapter builds a device module (open question 1).                                                                                                                                                                                                                                                                                                                                                    |
| `[geo]` unset or `platform` on Axum or Spin                                                                                   | Valid, and the host lookup resolves nothing there, so the jurisdiction is unknown for every request. Cloudflare reads a location from the `cf-` request headers, where `cf-ipcountry` comes on every plan and the region only with Cloudflare's Enterprise visitor-location transform. Fastly uses its own geolocation.                                                                                                                              |
| An EC module is selected and no location is ever resolved (`[geo] module = "none"`, or Axum or Spin)                          | Valid, and no identifier is ever created, because an unknown jurisdiction fails the gate (section 5). No startup check warns about the pairing, and the consent pipeline logs one runtime warning when a request has no geo (open question 2). Row 4 changes the default and adds the `assume_single_jurisdiction` check.                                                                                                                            |

Vendor module tables deserve their own note. An `[ec.<name>]` table that names an
implementation core does not build is kept as raw values
(`EcModuleSettings::Injected`), and the adapter that injects the module
deserializes the table into the module's own config type. Core names no Edge
Cookie vendor, so a new vendor module adds no vendor name to core, and no adapter
in this repository injects one today. The module's own crate rejects settings it
does not know when it deserializes, which is the §2.1 rule applied by the party
that owns the settings.

No setting forces an identity-graph store to exist when a module is selected.
`[ec] ec_store` stays optional, because the portability adapters run without
platform KV. Without a store the organic path can create an identifier for the
request, and EC finalization then writes no cookie and logs that the graph is
unavailable, so no cookie exists without a row. Row 5's resolve endpoint refuses
to create when no graph is available, because a cookie without a row could never
be withdrawn through the graph.

### 6.1 Module switching: what a switch actually does

Switching `[ec] module` **retires every identity the previous module created**.
This section says exactly what that means, because a deployer has to plan around
the retirement rather than discover it.

No continuity across a switch is built, so a switch of any kind retires the
previous population (section 11 lists what no row builds). Ownership is decided
on the code prefix **before** any module is asked about the shape, so a newly
selected module rejects every identifier the previous one created, whatever its
shape, because the code differs. The pre-envelope bare form is the one case
without a code, and it belongs to `hmac` alone.

What a switch does, precisely:

- **Read-back.** Every identifier carrying the retired module's code is treated
  as absent. Such an identifier never becomes the request's active identity,
  never goes onward to a partner, and is rejected on the pull-sync, batch-sync
  and admin paths too. This is the §5 guarantee and it is the half of the
  behavior that matters most, which is that two modules' identity populations can
  never mix.
- **The browser cookie.** A later withdrawal still expires the `ts-ec` cookie,
  because that path keys off the raw cookie being present rather than off who
  owns it. The browser stops carrying the retired identifier.
- **The identity-graph rows.** A later withdrawal does **not** tombstone the
  retired module's rows. Core cannot derive their keys, because the key form is
  the owning module's own normalization and the owning module is no longer
  configured. Those rows stay as they are until their one-year entry lifetime
  (`ENTRY_TTL`) ends. Tests pin this behavior
  (`switching_module_leaves_the_previous_modules_row_beyond_withdrawal` in
  `ec/finalize.rs` and `a_foreign_module_code_is_treated_as_absent` in
  `ec/mod.rs`).
- **The `ts-ecr` client-cycle marker.** Row 5 adds this marker. The marker
  carries no identity, so the `{code}~` envelope does not namespace it and a
  switch would otherwise leave it standing with its long `Max-Age` intact. Row 5
  expires the marker on any request carrying a `ts-ec` the selected module does
  not own, using the same ownership test as read-back, so a visitor whose
  identifier has just become unrecognized is not left behind a marker that tells
  the page script a resolve has already succeeded. The client-cycle spec states
  the rule in full.

**What a deployer must do about revocation.** Treat a module switch as a one-way
retirement of the identity population, and deal with the previous module's rows
before or alongside the switch, not after. A withdrawal that arrives after the
switch clears the browser but leaves the row. The rows of a retired module share
its `{code}~` key prefix, so the platform's own KV tooling can list and clear
them by that prefix, with one exception. A retired `hmac` module also wrote bare
rows before the envelope existed, keyed `<64 hex>.<6 alphanumeric>` with no
prefix, so clearing the `hmac~` prefix leaves those bare rows live and revocable.
Either clear every row of the retired module at the switch, or accept that the
rows persist until the one-year lifetime ends and that withdrawals arriving in
the meantime are recorded only in the browser. Do not switch modules while
identities are live that the deployment may still be obliged to revoke in the
identity graph.

### 6.2 Runtime failure modes

Startup validation covers configuration. The cases below cover a healthy
configuration meeting an unhealthy runtime. Behavior on this branch, each case
logged, none silent:

| Failure                                                            | Behavior                                                                                                                                                                                                                                                                                                              |
| ------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `generate` returns an error                                        | No identity this request. The organic caller (the Fastly publisher fallback or an integration proxy route) logs at error level and the request proceeds stateless. No cookie is written.                                                                                                                              |
| A module creates an identifier outside the global bounds           | Rejected at create, never rewritten. The organic path yields no identity, with an error logged. Row 5's resolve endpoint answers 400.                                                                                                                                                                                 |
| A module asks for a response header inside core's reserved surface | Creation fails as it does for a bounds violation, and neither the identifier nor any header from that module response is kept. The reserved surface is a `Set-Cookie` for a `ts-` cookie, any `x-ts-` header, a framing or hop-by-hop header, and `cache-control` (`reserved_response_effect` in `ec/module.rs`).     |
| The identity graph already holds the candidate's key               | The module is asked again, up to five attempts, and then creation fails with an error.                                                                                                                                                                                                                                |
| Identity-graph write fails at create                               | The create is undone (no identifier, no cookie), with the error logged. The next eligible request retries. Row 5's resolve endpoint answers 503.                                                                                                                                                                      |
| The `host_signals` module finds no TLS/HTTP-2 signals              | Defers with a warning. No identity this request, and no degraded IP-only identifier is created under the `host_signals` name.                                                                                                                                                                                         |
| The `fastly` device module sees no TLS JA4 signal                  | The request is classified as not a browser, because `looks_like_browser` needs both a JA4 class and a platform class. The bot gate then gives it no identity-graph access, so no row and no cookie are written for it, except that a withdrawal still reaches the graph.                                              |
| Geo lookup fails, or finds no location                             | Each adapter logs a warning for a failure and treats both cases as no location. The jurisdiction is unknown and creation is refused, which fails closed. An unknown jurisdiction is not a withdrawal, so an identifier the visitor already holds stays in the browser. Row 4 tells the two cases apart (`GeoStatus`). |
| Geo resolves a country or region that is in no regulation list     | The jurisdiction is non-regulated and creation is allowed.                                                                                                                                                                                                                                                            |
| A withdrawal's graph tombstone fails                               | The cookie is already expired in the response, so the row may stay live with consent granted, and a warning says so (`ec/finalize.rs`).                                                                                                                                                                               |
| An incoming cookie value fails the bounds at read-back             | Treated as absent, with a warning naming the source.                                                                                                                                                                                                                                                                  |

### 6.3 Storage contract

The module-code registry is built. Codes are allocated in
`provider-code-registry.md`, carried as the `{code}~` prefix of every created
identifier, and therefore present in every new graph key. The key uses
a tilde separator and not fixed-width segments with no delimiter, because
pre-envelope bare identifiers remain deployed and a code such as `1a2b` is valid
hex, so delimiter-free parsing could misread a bare identifier.

Storage today keys the identity graph with `module_kv_key`, which is the
identifier's code, then `~`, then the selected module's `normalize_id_for_kv` of
the value part. A bare identifier has no code, so its key is the module's
normalization of the whole value. For the built-in HMAC module that is the
identifier with the hash segment lowercased, so a new row is keyed
`hmac~<hash>.<suffix>` and a row written before the envelope keeps its bare key
`<hash>.<suffix>`, and every earlier row stays reachable (section 3 says what
that does to `cluster_size`). For an opaque module the key is `{code}~` plus the
identifier's value part. Rows carry the JSON shape they carried before these
rows, including the optional `device` record (`KvDevice`, with `is_mobile`,
`ja4_class`, `platform_class`, `h2_fp_hash` and `known_browser`), which the
built-in device module fills only in part because it leaves `ja4_class` and
`h2_fp_hash` empty. A row lives one year (`ENTRY_TTL`), the same as the cookie's
`Max-Age`. Record classes for family revocation, authority state, a
negative-intent outbox, rowless withdrawal and deployment metadata are not built
(section 11).

## 7. Composition root and adapter parity

Module construction happens in one place per concern, in core, and every adapter
calls it. That place is the composition root, meaning the code that decides which
implementation is built. No adapter wires a concrete implementation directly into
the request path, except that the Fastly adapter supplies the Fastly pieces (the
host-signal service and the device module) that the core builders ask for:

- `build_module` (`ec/module.rs`) constructs the selected EC module, injecting
  the host's `HostSignals` when supplied and matching an adapter-injected vendor
  module by its `id()`. Fastly, Cloudflare and Spin resolve it once while they
  build application state, through `build_reusable_module`, and give the result
  to each request's `RuntimeServices` (`resolved_ec_module`). Axum only checks
  (`ensure_module_available`), and `EcContext` resolves the module on each
  request. A module built from one request's evidence (`host_signals`, reported
  by `is_request_scoped`) is never kept, so `build_reusable_module` returns
  nothing for it and each request builds it from its own signals. Inside a
  request, `EcContext` holds one module and uses it for read-back, for the
  gate's test that a module is selected, and for creating. Other callers, which
  are the egress helper `recognized_ec_id` and the Fastly admin lookup and
  batch-sync handlers, call `request_module`. That function returns the module an
  adapter handed over when there is one and builds another otherwise.
- `build_device_module` (`ec/device.rs`) returns the `builtin` classifier unless
  `fastly` is selected, in which case it calls the factory that the Fastly
  adapter passes in, which builds `FastlyDeviceModule` over `FastlyHostSignals`.
  Only the Fastly entry point calls it (`derive_device_signals` in `main.rs`).
- `build_geo_module` (`platform/mod.rs`) returns the adapter's host geo
  implementation unless `none` is selected, in which case it returns
  `DisabledGeo`. All four adapters route their host geo through this selector
  when they assemble their runtime services, and the Fastly entry point uses it
  a second time for the `x-geo-*` headers it writes on every response, so
  `none` stops that lookup too.

All four adapters read the EC state with `EcContext::read_from_request_with_geo`,
after each has looked up the location itself. A failed lookup is logged as a
warning and treated as no location. What each adapter wires:

| What the adapter does                                 | Fastly                     | Axum                     | Cloudflare                        | Spin    |
| ----------------------------------------------------- | -------------------------- | ------------------------ | --------------------------------- | ------- |
| Checks `[ec] module` when it builds application state | Yes                        | Yes                      | Yes                               | Yes     |
| Keeps the resolved module for its requests            | Yes, except `host_signals` | No, resolves per request | Yes                               | Yes     |
| Supplies host signals, so `host_signals` can build    | Yes                        | No                       | No                                | No      |
| Builds the device module                              | Yes                        | No                       | No                                | No      |
| Routes geo through `build_geo_module`                 | Yes                        | Yes                      | Yes                               | Yes     |
| What `platform` geo resolves                          | Fastly's geolocation       | Nothing                  | A location from the `cf-` headers | Nothing |
| Creates identifiers and writes the cookie             | Yes                        | No                       | No                                | No      |
| Registers the EC API routes (identify and batch-sync) | Yes                        | No                       | No                                | No      |

The cross-adapter parity suite (`geo_header_parity_on_all_responses` in
`trusted-server-integration-tests/tests/parity.rs`) checks that the
`X-Geo-Info-Available` header is present on responses from Axum, Cloudflare and
Spin, and equal between Axum and Cloudflare. The suite sets no `[geo]` or
`[device]` selector, so it exercises neither. The EC API routes are Fastly-only
today, as section 1 notes, and the Spin adapter's route list records why.

## 8. Crate layout and CI

Host and vendor module crates live in nested directories grouped by capability,
with flat package names following the existing convention:

- `crates/device/fastly` is package `trusted-server-device-fastly`.
- `crates/geo/fastly` is package `trusted-server-geo-fastly`.
- `crates/edgecookie/<vendor>` is the documented home for vendor EC crates. The
  directory currently holds only a README, because the built-in modules live in
  core and no vendor crate exists yet.

Nested directories scale per vendor as modules multiply, package names carry the
flat convention, and the README stakes out the vendor location before the first
vendor crate lands.

Every new crate is in the `.cargo/config.toml` aliases (`check-fastly`,
`clippy-fastly`, `test-fastly`, `build-fastly`), so the module crates are linted
with `-D warnings` and tested by the same gates as every other workspace member.
The two Fastly crates hold no tests of their own, and `DeviceSignals::derive`,
which the Fastly device module calls, is tested in core.

## 9. Behavior preservation notes

Two defaults change effective behavior on existing Fastly deployments across the
stack, and only the first is on this branch. Both are called out in the migration
spec and must be prominent in release notes. The `[Unreleased]` section of
`CHANGELOG.md` has no entry for either change yet.

- **Bot gate (this branch).** The pre-module EC bot gate required JA4 and
  platform class. The default `builtin` classifier is User-Agent only, so the
  gate is weaker by default. The stronger gate is available as
  `[device] module = "fastly"`. Release notes must call out the weaker default
  rather than presenting selection alone as authorization.
- **Geo (row 4).** On this branch geo resolution is unchanged, because the host
  lookup stays the default. Row 4 flips the default to no location. With no geo
  module, jurisdiction resolution then falls to the required top node of the
  `permissions.yaml` rules tree. The permission model constrains the combination
  so it cannot silently grant permissions to mis-attributed traffic. The top node
  must carry a `group` naming a defined group and a `jurisdiction`, a deployment
  running an EC module without geo must set `assume_single_jurisdiction = true`,
  and a failed lookup resolves to the requires-signal floor instead of that
  baseline. The flip and those constraints land in the same row, so the flip
  never ships without them.

**Rollout order for this branch.** A configuration written for the previous
release still loads, because `[ec] passphrase` migrates to `module = "hmac"` with
a deprecation warning, so the binary can be deployed first and the configuration
moved to `[ec] module` and `[ec.hmac]` afterwards. A rollback needs the reverse
care. The previous release's `[ec]` section refuses keys it does not know, so it
rejects a configuration that carries `[ec] module` or `[ec.hmac]`. Restore the
old-shape configuration first, which this branch's binary still migrates, and
roll the binary back after. The previous release also accepts only the bare
identifier shape (`is_valid_ec_id`), so it does not recognize an `hmac~` cookie
and cannot reach a row keyed `hmac~...`.

## 10. Testing strategy

Built, in the crates named:

- Round-trip tests with a non-default module, proving an opaque identifier
  survives read-back byte for byte and that its graph row is written under the
  key built from its own code (`ec/mod.rs`). Row 5 adds the client-cycle scenario
  in `ec/resolve.rs`.
- Delegation tests proving the injected-module wrapper forwards `accepts_id` and
  `normalize_id_for_kv` to the inner module, so a vendor identifier is never
  dropped by the built-in defaults (`ec/module.rs`).
- Gate tests in `consent/mod.rs` and `ec/consent.rs` proving that a request with
  an unknown jurisdiction creates no identifier, and finalization tests in
  `ec/finalize.rs` for what happens when creation is not permitted. Row 4 adds
  the tests for the `required_permissions` gate.
- Settings validation tests covering the section 6 table: the missing table, the
  table without a selector, explicit `none`, the stray table, unknown selector
  keys for device and geo, unknown fields in the module sections, and the
  deprecated passphrase migration with its both-forms rejection. Row 4 adds the
  tests for top-node validation of the rules tree and the jurisdiction
  acknowledgment.
- Geo builder tests (`platform/mod.rs`) showing the default selects the host geo,
  `none` selects no geo, and `platform` selects the host geo. A device builder
  test (`ec/device.rs`) shows the default selects `builtin` and `fastly` builds
  the module the adapter supplies.
- Host-signals module tests covering creating from host signals, deferring
  without them, and the startup refusal of a selected host-signals module on a
  host that supplies none.
- Switching tests, named in section 6.1.
- Host signal header tests proving that the Lockr API proxy
  (`lockr_api_proxy_does_not_forward_host_signal_headers`), each of the five
  Permutive proxy routes (`permutive_proxy_does_not_forward_host_signal_headers`)
  and the publisher origin fetch, streamed or not
  (`publisher_origin_fetch_drops_host_signal_headers`), drop `x-ts-tls-ja4` and
  `x-ts-h2-fingerprint` while still forwarding the reader's other headers, and
  a test that every host signal header is an internal header
  (`every_host_signal_header_is_an_internal_header` in `constants.rs`).

Not covered by any test on this branch:

- An adapter routing its geo through `build_geo_module`. Nothing fails if an
  adapter stops doing so.
- `[device] module = "fastly"` on an adapter other than Fastly.
- The Fastly entry point setting `x-ts-tls-ja4` and `x-ts-h2-fingerprint`, and
  the adapter building its `HostSignals` service from them. No adapter test
  names either header.
- The two Fastly crates, which hold no tests.

Row 4 and row 5 add the tests for the work they build.

## 11. Implementation order

The stack builds this design in rows. Rows 2 and 3 are on this branch:

- **Row 2, the Edge Cookie module seam (IABTechLab/trusted-server#1343).** The
  trait with recognition and KV normalization, the global identifier bounds, the
  module-code envelope, selection and validation, the injected-module path, the
  deprecated-passphrase migration, and the round-trip proof with a non-default
  module.
- **Row 3, device and geo selection (IABTechLab/trusted-server#1344).**
  `DeviceModule` with the `builtin` default and the opt-in Fastly host-evidence
  module, `PlatformGeo` selection with the host-lookup default and the `none`
  opt-out, the Fastly adapter's host-signal service, the opt-in `host_signals` EC
  module (carrying the open question of section 2), and the two Fastly crates.

Later rows build what this page names and this branch lacks:

- **Row 4, the permission model (IABTechLab/trusted-server#1345).**
  `required_permissions` on the three traits and the gate that enforces the EC
  module's declaration, `IdentityInput.permissions`, the top node of the rules
  tree and the jurisdiction acknowledgment (`assume_single_jurisdiction`), the
  failed-lookup floor (`GeoStatus` and
  `EcContext::read_from_request_resolving_geo`), the `permission-signal` type,
  and the flip of the geo default to no location.
- **Row 5, the client-set Edge Cookie value path
  (IABTechLab/trusted-server#1346).** `resolve_from_client` and
  `ClientResolveInput`, the `ts-ecr` marker, the resolve endpoint, and the
  `client_fixed` demonstration module behind the `client-fixed-demo` feature.
- **Row 7, the integration seam (IABTechLab/trusted-server#1348).**
  `with_ec_module`, `with_device_module` and `with_geo_module` on the
  registration builder, the `demand` and `ad-server` types, and the refusal of a
  nonempty `required_permissions` from a module-supplied device module.

No row of the stack builds the following:

- A `parse` that turns a module's identifier into a typed value, a graph key
  suffix and a cluster prefix that the module declares, and a reusable
  conformance suite driven by fixtures.
- Module versions with `mint_version` rotation, and an ordered
  `legacy_providers` list of earlier readers that would carry identities across
  a switch. The configuration keys for them are refused as unknown keys.
- An identity activation protocol, classification of a cookie that has no row,
  and a typed boundary for identity that leaves the edge (`AuthorizedIdentity`
  and `RedactedRequestView`).
- Record classes for family revocation, authority state, a negative-intent
  outbox, rowless withdrawal and deployment metadata, with wire schemas and
  known-answer vectors and a per-field contract for graph rows.
- An adapter capability matrix declaring per-record-class consistency,
  durability and retention proofs, and activation and lease qualification, with
  the `cluster_fallback` degradation policy that would use the cluster
  capability. The matrix's motivating rule stands for any later work, which is
  that "has KV" says nothing about whether a revocation is observable, so
  eligibility for identity features must be declared and checked, not assumed.
- A startup error for an Edge Cookie module that creates identifiers when no
  identity-graph store is configured.

## 12. Open questions

The code decides none of the following, and this spec does not decide them
either.

1. **A selection an adapter cannot satisfy.** An Edge Cookie module that an
   adapter cannot supply stops startup. `[device] module = "fastly"` on Axum,
   Cloudflare or Spin is accepted and nothing reads it, and `platform` geo on
   Axum or Spin resolves nothing. Whether those adapters should refuse these
   selections, as they refuse `host_signals`, is open.
2. **Edge Cookie without a location.** With an `[ec]` module and either
   `[geo] module = "none"` or an adapter whose host lookup resolves nothing, the
   jurisdiction is always unknown, no identifier is ever created, and nothing at
   startup says so. Row 4 changes the default and refuses the pairing unless
   `assume_single_jurisdiction` is set. Whether this branch needs a check of its
   own before row 4 merges is open.
3. **The end of the bare form.** The `hmac` module reads the pre-envelope bare
   identifier with no end date, and nothing rewrites a bare cookie or row as a
   coded one. A comment in the cookie lifetime test in `ec/cookies.rs` refers to
   a retirement condition for the bare reader that `module_owns_id` does not
   state. When the bare form stops being read is open.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-07-31 | First draft.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| 2026-08-25 | Revised against the code of the first two rows, which had landed as #1043 and #1044, with permission enforcement in #1045.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| 2026-09-01 | The fallback baseline for a request with no resolvable country moved to the top node of the `rules:` tree in `permissions.yaml`, where `jurisdiction` is a per-node inherited attribute, and `[geo] default_country` was retired. The two `[consent]` applicability lists retire into the same tree (permission-model spec §3.2, §3.4 and §12).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| 2026-10-06 | What an operator selects is a module, and the selector key reads `module` (§2.1).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| 2026-10-15 | Fixes from review round 1. The page now describes the code on `split/2-device-geo` and names the row that builds each later feature, so the permission checks, the browser round trip, the registration hooks and the flip of the geo default are no longer described as built here. The geo default, the passphrase key, the Edge Cookie trait, the identity graph key, the parity of device and creation between adapters and the host signals capture are corrected. The selector key reads `module` or `modules` throughout §2.1. An adapter table, the missing settings, failure cases and rollout order, and a list of open questions are added. The history of earlier revisions moved into this table, and the former sections 12 and 13, which compared the page with issue #778 and with the first draft, are replaced by the open questions and this table. |
| 2026-10-16 | The host signal headers `x-ts-tls-ja4` and `x-ts-h2-fingerprint` are internal headers, so the Lockr and Permutive proxies drop them and the publisher origin request removes them, with a test for each path, and the open question about them is closed (§5, §10, §12). From review round 2, what each adapter answers when its startup check fails and that the list of implementations appears only in the log (§6).                                                                                                                                                                                                                                                                                                                                                                                                                                                |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against
   `split/2-device-geo` at `138faf1c1`. Issues found, seven of them blocking. The
   page described the permission checks and the browser round trip as built when
   rows 4 and 5 build them, said the geo default is no location when the default
   is the host's lookup, said core names none of the implementations and that a
   registration builder supplies them when core names six and the builder has no
   hook for a module, gave a configuration key `passphrase_secret` that the code
   refuses, said all four adapters build the device module and create identifiers
   when only Fastly does, said the identity graph key is the module's value alone
   and that every row of a retired module shares the `{code}~` prefix when new
   keys start with the prefix and rows written before the envelope have none, and
   called the TLS and HTTP/2 signals opt-in when the Fastly entry point sets them
   on every request. This revision states what the code on `split/2-device-geo`
   does, names the row that builds each feature the page described too early,
   corrects the partly true claims, adds the adapter table, the missing settings
   and failure cases and a rollout order, and drops the history of earlier
   revisions, while the four questions under Open questions wait for code
   changes.
2. **Round 2, 2026-10-16.** A lighter review in a fresh session, checking the
   round 1 fixes against `split/2-device-geo` at `138faf1c1`. Issues found, none
   blocking. This revision corrects what each adapter answers when its startup
   check fails, and says that the list of implementations appears only in the
   log.
