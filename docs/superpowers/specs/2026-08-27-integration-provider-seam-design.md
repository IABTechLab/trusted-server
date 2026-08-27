# Design Spec: The Integration Provider Seam

**Status:** Proposed, 2026-08-27. Sixth PR in the provider series. It adds
this document only and targets `main` directly; it reads alongside the
series' specs, which land with PR #1047.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-07-30-pluggable-providers-design.md`,
`2026-07-30-provider-migration-rollout-design.md`,
`provider-code-registry.md`
**Related PRs:** #1043, #1044, #1045, #1046, #1047, #1054
**Last updated:** 2026-08-27

> **Why this spec exists.** PRs #1043 to #1047 open the identity, device and
> geo seams, so a vendor can ship an Edge Cookie provider in its own crate
> and an adapter injects it. The nine vendor integrations already inside
> `trusted-server-core` do not sit behind those seams. They hang off the
> integration registry, which is a private table in core, so none of them
> can move out until that table is opened. This spec defines the one core
> change that opens it, so the migration of every existing vendor is a
> single defined piece of work rather than an open question repeated once
> per vendor.

## 1. The problem, with the code that causes it

Every claim here was read from the tree at `split/5-response-hook-docs`,
which is `main` plus the five PRs.

1. **The registry is closed.** `IntegrationRegistry::new` takes only
   `&Settings` and iterates a fixed table
   (`crates/trusted-server-core/src/integrations/registry.rs:797`, table at
   `crates/trusted-server-core/src/integrations/mod.rs:290`). Both
   `IntegrationBuilder` and `builders()` are `pub(crate)` with private
   fields, so no adapter and no external crate can add to the list. The
   payload type `IntegrationRegistration`
   (`registry.rs:586`) is already public, so an outside crate can build a
   registration but has nowhere to hand it.
2. **Browser JavaScript is fixed at build time.** `trusted-server-js`
   discovers `lib/src/integrations/*/index.ts`, builds one file per
   integration, and `build.rs` writes a fixed array of `include_str!`
   entries consumed by `bundle.rs`. `IntegrationRegistry::js_module_ids`
   only serves a module when
   `trusted_server_js::module_bundle(id).is_some()`
   (`registry.rs:1169`), so an integration outside that compile-time map
   gets no script however it registers.
3. **Startup validation names every vendor.** `validate_enabled_integrations`
   imports and calls each vendor's config type by name
   (`crates/trusted-server-core/src/config.rs:136` to `:166`).
4. **Auction providers are a second closed table.**
   `crates/trusted-server-core/src/auction/mod.rs:49` lists Prebid, APS and
   the ad server mock.
5. **Two vendors reach further into core.** DataDome drives cache privacy
   and the origin fetch decision through a marker type
   (`html_processor.rs:303`, `publisher.rs:4367` to `:4387`,
   `publisher.rs:2653`), and GPT diagnostics is called by name from all four
   adapters (for example
   `crates/trusted-server-adapter-fastly/src/app.rs:584`).

The result is that the project carries nine vendors as core code (ten
registered integrations, since GPT registers a proxy and a diagnostics
integration). Tech Lab engineering time is spent on named commercial vendors, and
every new vendor is another core change, as PR #1054 shows.

## 2. Principle

A vendor integration is a provider like any other. Core owns the seam and
owns nothing behind it. Concretely:

- Core defines the registration contract and the request pipeline. It names
  no vendor.
- A vendor integration ships as its own crate with its Rust, its browser
  JavaScript, its configuration type, its startup validation and its tests.
- An adapter composes the deployment by injecting the registrations it was
  built with, exactly as it already injects the geo and device providers.
- Tech Lab engineering assesses and reviews vendor crates. It does not
  maintain them.

## 3. Design

### 3.1 Opening the registry

Make the builder contract public and give the registry a second input.

- `IntegrationBuilder` becomes `pub` with a public constructor, and
  `builders()` stays as the built-in set.
- `IntegrationRegistry::new` gains a companion,
  `IntegrationRegistry::with_registrations(settings, extra)`, where `extra`
  is a slice of externally supplied builders. `new` keeps its signature and
  calls the companion with an empty slice, so no existing caller changes
  behavior.
- Duplicate integration ids are a startup error, naming both sources, so a
  vendor crate cannot silently shadow a built-in.

### 3.2 Carrying browser JavaScript on the registration

Add one optional field to `IntegrationRegistration`, next to `js_deferred`
and `js_disabled`: the module source and its hash, both `&'static str`, so a
crate can `include_str!` its own built bundle.

`js_module_ids` keeps serving built-in ids from the compile-time map and
serves a carried module from the registration. `publisher.rs` composes the
served script and its hash from both sources rather than calling
`trusted_server_js::concatenate_modules` alone. The hash rule is unchanged,
so the served bundle stays cacheable and its integrity attribute stays
correct.

### 3.3 Startup validation on the registration

Replace the named list in `config.rs` with a validation hook on the
registration, so a vendor validates its own configuration and a missing
vendor cannot silently stop being validated. The existing test that asserts
every registered integration is covered by deploy validation
(`config.rs:408`) is rewritten against the hook, so the guarantee survives
in a vendor-neutral form.

### 3.4 Auction providers

Give `AuctionOrchestrator` the same treatment as the registry: a public
provider-builder type and a second input, so an auction-side vendor such as
APS can register from its own crate. Prebid stays in core as protocol
support rather than as a vendor integration.

The bid renderer contract is generalized in the same change. Today
`BidRenderer` is an enum with one variant, `Aps(ApsRendererV1)`
(`crates/trusted-server-core/src/auction/types.rs:216`), serialized into the
OpenRTB response extension under a `type` tag
(`crates/trusted-server-core/src/auction/formats.rs:377`,
`crates/trusted-server-core/src/openrtb.rs:183`). It becomes an open
descriptor, a type tag and a payload the auction provider supplies, with
the same serialized form, so the response a page receives does not change
and the APS renderer type moves with APS. The ad server mock in core uses
the neutral form.

### 3.5 The two neutral hooks

- **Response shaping.** DataDome's marker becomes a neutral request
  extension meaning "this response is personalized to the request, do not
  share it", set by any integration. Core keeps the behavior, being the full
  body buffer and private cache, and stops naming a vendor.
- **Request prepare and finalize.** The direct GPT diagnostics calls in the
  four adapters move behind hooks on the registration, so an adapter runs
  whatever its registrations declare.

## 4. Migration of the nine existing integrations

One vendor per PR, after this change lands. Each moves its Rust, its
TypeScript, its config type and its tests into
`crates/integrations/<vendor>`, and the adapter that wants it depends on it.

| Vendor                                                           | What it needs                                                                                                           |
| ---------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| Didomi, Google Tag Manager, Lockr, Osano, Permutive, Sourcepoint | Move as they are. Coupled only through the builder table, deploy validation and the JS map.                             |
| APS                                                              | Also needs the auction provider seam and the generalized renderer contract in §3.4, both of which this change delivers. |
| GPT (the `gpt` proxy and `gpt_diagnostics`)                      | The proxy moves as it is. The diagnostics half needs the prepare and finalize hooks in §3.5.                            |
| DataDome                                                         | Needs the neutral response-shaping hook in §3.5, and about forty test literals move with it.                            |

`[integrations.<vendor>]` configuration tables need no change, because
`IntegrationSettings` is a flattened map that already accepts unknown vendor
keys (`crates/trusted-server-core/src/settings.rs:166`).

## 5. What does not change

The request pipeline, the hook traits and their order, the served script
format and its hash, every `[integrations.*]` table, the permission model,
and the Edge Cookie, device and geo seams from PRs #1043 to #1046. No
integration changes behavior. A deployment that lists the same integrations
gets the same responses.

## 6. Acceptance

1. **A round trip with a non-default implementation.** A test integration
   defined outside `trusted-server-core`, carrying its own JavaScript,
   registers through an adapter, appears in the served bundle with the
   right hash, runs its hooks in the right order, and is rejected on a
   duplicate id. A seam is only proven by an implementation that is not the
   built-in one.
2. **Parity.** The existing integration and parity suites pass unchanged,
   because the built-in set still registers through the same path.
3. **No vendor left behind.** The rewritten deploy-validation test shows
   every registered integration validates its configuration.
4. All CI gates in `CLAUDE.md`, on all four adapters.

## 7. Risk

The change is wide but shallow. It touches the registry, the served script
path, deploy validation and four adapter entry points, and it changes no
integration's behavior. The largest risk is the served script, where a
mistake shows up as a wrong hash or a missing module, so §6's round trip
covers both. Doing this once is what removes the per-vendor core change
that the project pays for today, most recently in PR #1054.

## 8. Sign-off

| #   | Decision                                                                        | Status               |
| --- | ------------------------------------------------------------------------------- | -------------------- |
| 1   | Vendor integrations belong outside core, behind the registration contract       | Proposed             |
| 2   | Tech Lab engineering reviews vendor crates, and does not maintain them          | Proposed, governance |
| 3   | A registration may carry its own browser JavaScript                             | Proposed             |
| 4   | Deploy validation moves onto the registration                                   | Proposed             |
| 5   | The nine existing integrations migrate one PR each, on the schedule in §4       | Proposed             |
| 6   | This change is complete in itself: after it, no vendor move needs a core change | Proposed             |

## Revision record

| Date       | Change                                                                                                                        |
| ---------- | ----------------------------------------------------------------------------------------------------------------------------- |
| 2026-08-27 | First draft, written against `split/5-response-hook-docs`.                                                                    |
| 2026-08-27 | Brought the bid renderer contract into scope (§3.4), so that after this change no vendor move needs a core change (§8 row 6). |
