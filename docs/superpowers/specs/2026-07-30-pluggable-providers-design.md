# Design Spec: Pluggable Edge Cookie, Device, and Geo Providers

**Status:** Draft
**Author:** Engineering
**Issue references:** #777, #778, #780, #781
**Related specs:** `2026-07-30-permission-model-design.md`,
`2026-07-30-provider-migration-rollout-design.md`,
`2026-07-30-client-cycle-ec-resolve-design.md`
**Last updated:** 2026-07-30

> **Context.** PR #838 proposed a first implementation of this epic in a single
> change. Review of that PR surfaced design gaps this spec exists to close
> before a second implementation pass: an identity abstraction that owned
> minting but not recognition, per-adapter divergence in provider selection,
> silent misconfiguration modes, and speculative trait surface with no
> production caller. This spec is the authoritative statement of what the
> provider architecture must do; where it contradicts PR #838, this spec wins.

---

## 1. Overview and goals

Trusted Server makes three per-request data decisions that are currently
hard-wired: whether to create or keep an Edge Cookie (EC) identity, how to
classify the requesting device, and whether to resolve geolocation. Each
becomes a **provider**: a selectable component chosen in operator
configuration, with a deliberately neutral default.

Goals:

- A deployment picks an implementation per concern (including none) without a
  code change to Trusted Server core.
- Defaults are neutral: with no configuration, no EC is created, device
  classification uses only the User-Agent, and no geolocation is performed. A
  default deployment makes no third-party or host-specific call.
- A provider **declares** the permissions its data use requires (see the
  permission model spec); **core enforces** that declaration. A provider
  cannot authorize itself.
- All adapters (Fastly, Axum, Cloudflare, Spin) behave identically for
  identical configuration, or fail loudly at startup where a host cannot
  satisfy the selected provider.

Non-goals:

- No vendor provider ships in this epic beyond the host-platform
  implementations named below.
- The client-cycle (browser round-trip) provider type is **out of scope**
  here; it has its own spec and must clear that spec's requirements first.

## 2. Provider taxonomy

| Concern     | Trait                | Built-in default            | Opt-in host implementation                                        |
| ----------- | -------------------- | --------------------------- | ----------------------------------------------------------------- |
| EC identity | `EdgeCookieProvider` | none (stateless)            | `hmac` (in core; HMAC over client IP, preserves today's identity) |
| Device      | `DeviceProvider`     | `builtin` (User-Agent only) | `fastly` (JA4 / HTTP-2 fingerprints)                              |
| Geo         | `GeoProvider`        | none (no location)          | `platform` (host geo lookup)                                      |

Selection keys are strings in operator configuration:

```toml
[ec]
provider = "hmac"

[ec.providers.hmac]
passphrase = "example-passphrase"

[device]
provider = "builtin"

[geo]
provider = "platform"
```

## 3. The identity lifecycle contract

This is the section PR #838 lacked, and the source of its most structural
defect: the trait abstracted **minting** an identifier but left
**recognition** (`is_valid_ec_id`), **hashing** (`ec_hash`), and **KV key
normalization** hard-coded to the built-in HMAC shape. Any provider whose
identifiers do not match `{64hex}.{6alnum}` minted cookies that the very next
request discarded, and whose identities could never be tombstoned on
withdrawal.

An `EdgeCookieProvider` owns the **complete lifecycle** of the identifiers it
mints. Every lifecycle operation core performs on an EC value MUST be routed
through the selected provider:

| Lifecycle operation  | Where core uses it today                                         | Contract                                                                                                                                          |
| -------------------- | ---------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Mint**             | EC generation on first eligible request                          | Provider returns the identifier (and only core writes the cookie).                                                                                |
| **Recognize**        | Reading `ts-ec` back from the request; deciding `ec_was_present` | Provider validates that a returned cookie value is one of its identifiers. A value the selected provider does not recognize is treated as absent. |
| **Hash / normalize** | KV identity-graph keys, log redaction                            | Provider (or a provider-supplied codec) maps an identifier to its stable KV key form.                                                             |
| **Tombstone**        | Withdrawal: expiring the cookie and writing revocation markers   | The identifiers eligible for tombstoning are exactly those the provider recognizes — never a shape-gated subset.                                  |

**Invariant:** for every provider `P` and every identifier `id` minted by `P`,
`P.recognize(id)` is true, `P` produces a stable KV key for `id`, and a
withdrawal request carrying `id` tombstones it. A conformance test suite MUST
assert this round-trip for every shipped provider, and the suite MUST be
written so a future provider crate can run it against its own implementation.

## 4. Trait surface: minimalism rule

Every trait method MUST have at least one production (non-test) caller in the
same PR that introduces it. Speculative surface observed in PR #838 that MUST
NOT ship without a caller:

- `keys_equal` (no production caller; existed to serve a unit test),
- `GeneratedEdgeCookie::response_headers` (empty in all built-ins, plumbed
  through three layers),
- `IdentityInput.permissions` / `IdentityInput.consent` (ignored by all
  built-ins),
- `DeviceProvider::required_permissions` / `GeoProvider::required_permissions`
  **unless** the enforcement point of §5 lands in the same change.

If a future feature needs one of these, it arrives with that feature.

The minimal `EdgeCookieProvider` surface implied by §3 is:

```rust
pub trait EdgeCookieProvider {
    /// Stable configuration key ("hmac").
    fn id(&self) -> &'static str;
    /// Permissions this provider's data use requires. Enforced by core.
    fn required_permissions(&self) -> PermissionSet;
    /// Mint an identifier from request evidence.
    fn generate(&self, input: &IdentityInput<'_>) -> Result<EcId, Report<EcError>>;
    /// Whether `value` is an identifier this provider minted.
    fn recognize(&self, value: &str) -> bool;
    /// Stable KV key form of a recognized identifier.
    fn kv_key(&self, id: &EcId) -> KvKey;
}
```

(Names indicative; the shape is normative.)

## 5. Permission enforcement is core's job — for all three provider kinds

Before executing **any** provider (EC, device, or geo), core resolves the
request's permission set (see the permission model spec) and refuses to run a
provider whose `required_permissions()` are not all set. PR #838 declared this
method on all three traits but consulted it only for the EC provider; the
device and geo declarations were decorative. That is worse than absent — it
reads as a gate and is not one. The enforcement point MUST be a single shared
code path used by all three provider kinds, with a test per kind proving a
provider declaring an unset permission does not execute.

## 6. Selection, validation, and failure modes

All validation happens at **settings construction** — a misconfiguration is a
startup error, never a request-time error and never a silent behavior change.

| Configuration state                                                                                                                                  | Behavior                                                                                                                                                                                                                                    |
| ---------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `provider` names an unknown key                                                                                                                      | Startup error listing valid keys.                                                                                                                                                                                                           |
| `provider` set, its `[ec.providers.<key>]` block missing                                                                                             | Startup error.                                                                                                                                                                                                                              |
| `[ec.providers.<key>]` block present, `provider` unset                                                                                               | **Startup error.** (In PR #838 this silently ran stateless — the half-migrated config becomes a production identity outage detected by revenue drop. Rejecting it is the fix.) An operator who genuinely wants stateless deletes the block. |
| `provider` set to an implementation the running adapter cannot satisfy (e.g. a provider requiring host TLS fingerprints on an adapter that has none) | Startup error at adapter wiring time. Adapters declare their host capabilities to the composition root; the root checks the selected provider's needs against them **once**, at startup — not per request.                                  |
| No `provider`, no providers block                                                                                                                    | Valid: the neutral default for that concern.                                                                                                                                                                                                |

Unknown fields inside every provider config block are rejected
(`deny_unknown_fields` on all new settings structs — PR #838 applied it to
`Ec` but not to `EcProviders`, `DeviceConfig`, or `GeoConfig`, so a typo like
`providr` was silently ignored).

## 7. Composition root and adapter parity

Provider construction happens in exactly one place per concern
(`build_ec_provider`, `build_device_provider`, `build_geo_provider`), called
by **every** adapter. No adapter may wire a concrete implementation directly:
in PR #838 the Cloudflare adapter installed its host geo unconditionally,
so identical configuration produced different jurisdictions on different
adapters — which the permission model then turned into different privacy
outcomes.

Requirements:

- Each adapter's runtime-services setup routes through the shared builders.
- Providers are constructed **once** per application instance and stored in
  app state; PR #838 rebuilt the provider (cloning the secret into a fresh
  `Box<dyn …>`) up to three times per request.
- The cross-adapter parity suite gains cases asserting: (a) the selected
  provider is honored on every adapter, (b) the neutral default performs no
  host call on every adapter, and (c) a capability-unsatisfiable selection
  fails startup on the adapters that cannot satisfy it.

## 8. Crate layout and CI

Provider crates live flat under `crates/` following the existing naming
convention: `crates/trusted-server-geo-fastly`,
`crates/trusted-server-device-fastly`. (PR #838 introduced a nested
`crates/geo/fastly` layout that broke the directory–package correspondence
every other member follows.) No placeholder directories: a `crates/…/README.md`
with no crate ships when the first crate does.

Every new crate is added to the `.cargo/config.toml` aliases
(`check-fastly`, `clippy-fastly`, `test-fastly`, `build-fastly`) in the same
PR that adds the crate, and to the CI gate list in `CLAUDE.md`. PR #838's new
crates compiled only transitively and were never linted with `-D warnings`
nor had a single test.

## 9. Behavior preservation notes

Two defaults chosen for neutrality change effective behavior on existing
Fastly deployments; both are called out in the migration spec and must be
prominent in release notes:

- **Bot gate.** The pre-provider EC bot gate required JA4 _and_ platform
  class. With `device.provider = "builtin"` the gate degrades to User-Agent
  heuristics. Restoring the stronger gate requires `[device] provider =
"fastly"`; the migration guide lists this as a behavior-preserving step for
  Fastly deployments.
- **Geo.** With no geo provider, jurisdiction resolution falls to the
  configured default country. The permission model spec (§5) constrains this
  combination so it cannot silently grant permissions to mis-attributed
  traffic.

## 10. Testing strategy

- Provider conformance suite (§3 invariant) run against every shipped
  provider.
- Enforcement tests per provider kind (§5).
- Settings validation tests for every row of the §6 table, including the
  block-without-selector rejection.
- Parity suite additions of §7.
- Unit tests inside each provider crate; crates with no native-target tests
  still get clippy coverage via the alias wiring of §8.

## 11. Implementation order

1. Traits + lifecycle contract + conformance suite, `hmac` provider
   passing it (behavior-identical to today; see migration spec §3 for the
   ID-stability vectors).
2. Settings selection + validation table.
3. Composition root + all four adapters wired through it, parity cases.
4. Device and geo providers with the shared enforcement point of §5.

Each step is independently reviewable; none depends on the permission model
landing first (the EC gate keeps its current jurisdiction logic until the
permission model PR replaces it).
