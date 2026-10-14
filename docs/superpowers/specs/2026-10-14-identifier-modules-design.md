# Design Spec: A module hands core the identifiers it learned for a request

**Status:** Proposed, 2026-10-14. Implemented on a working branch of the fork, with no pull request raised yet.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-07-10-kv-eid-request-snapshot-ec-recovery-design.md`, `2026-09-08-1138-per-cookie-template-cache-policy-design.md`
**Related PRs:** none yet
**Issues:** IABTechLab/trusted-server#1337
**Last updated:** 2026-10-14

> **Why this spec exists.** Core parses the `ts-eids` cookie the Prebid page script writes, in both the shape it writes today and the one it wrote before, with the size limit the script applies, and syncs what it finds into the identity graph. So core knows one vendor's cookie, and the Prebid crate does not own the cookie its own script writes.

## 1. Problem

`crates/trusted-server-core/src/ec/prebid_eids.rs` holds the name of the cookie, under the public prefix, its two JSON shapes, its raw size limit, and the parser. The auction endpoints call the parser to fall back to the cookie when a request body carries no EIDs, the publisher path calls it for a navigation's and a page-bids request's auction, and the Fastly adapter extracts the cookie's raw value before routing so that response finalization can parse it afterwards, when the request is gone.

A second script that persisted identifiers in a cookie of its own, or a module that learned them from a store, would have no way to hand them to core short of a second parser in core.

## 2. Design

**Core declares the interface.** `IdentifierModule`, in `trusted_server_core::ec::identifiers`, is a module that learns identifiers for a request. It is called with a module call, as every module is, carrying the request's evidence, settings, services, consent and resolved permissions, and it names what it reads with `call.inject`. It returns OpenRTB-style `Eid` entries, in the order it offers them, so that where two carry the same partner the later one is the candidate. A call that withholds a value the module names contributes nothing and is logged, so one module's fault does not lose another's identifiers.

**A registration offers one.** `IntegrationRegistrationBuilder::with_identifier_module` declares it. No section selects one. Core asks every running registration's module, in registration order, so a module runs wherever its registration runs. The registry claims each module's name, so two registrations cannot declare the same one, and hands the list to each request's services through `RuntimeServices::with_identifier_modules`, as it hands the permission signal modules.

**Core asks once, when it reads the Edge Cookie state.** That is where the request's evidence is in hand and where the permission signal modules are already asked. The answers are held on the `EcContext` as `module_eids`, and from there they reach response finalization, which offers them to the identity graph, and the auction, which forwards them when the request body carries none. With no Edge Cookie module selected nothing can hold or forward an identifier, so none are asked.

**The order of the sources does not change.** Finalization collects partner updates from what the modules learned, then from the request body's own EIDs, which win on conflicts because a module's cookie is size-capped and may be missing partners the body still carries in full, then from the `sharedId` cookie. The auction forwards the body's EIDs, or what the modules learned when the body carries none, gated on the Edge Cookie as before.

**The Prebid crate owns its cookie.** `crates/auction/prebid/src/eids.rs` carries the cookie's name, its two shapes, its size limit and the parser, and `PrebidEids` is the first identifier module, registered from the Prebid builder. It reads the cookie through the request's evidence, named through the public prefix the services carry, so core names the cookie nowhere, as it names the Model Terms cookie nowhere.

**What leaves core.** `COOKIE_EIDS`, the parser and its tests, the cookie's extraction in the Fastly adapter, and the `eids_cookie` parameter of `ec_finalize_response`. The template cache's identity rule, which refused keying a shared template on the Edge Cookie and the EIDs cookie by name, refuses any cookie under the public prefix instead, because every cookie under the prefix is one reader's own state, and still refuses `sharedId`.

## 3. What is not in this change

- The `sharedId` cookie, which Prebid's SharedID module writes directly. Core still reads it by name, in `ec/identifiers.rs`, because it is not the cookie this change is about and because folding it into the identifiers a module hands core would forward it in bid requests, where today it reaches the identity graph alone.
- The OpenRTB `Eid` type the interface returns, which stays where core's auction keeps it.

## 4. Tests

- Core asks each module in order and keeps their order, passes a module the evidence it names, and skips a module whose call withholds what it names while the next still answers.
- Partner updates are collected from what the modules learned, the body's EIDs win over a module's stale value and ingest a partner the module's cookie trimmed, the `sharedId` cookie is applied last, an unknown source is skipped, an oversized identifier is skipped for a later valid one, and an empty registry touches no store.
- The auction falls back to what the modules learned when the body carries none, prefers the body when it carries EIDs, and resolves nothing when neither has any.
- The Prebid module reads the cookie named by the public prefix, learns nothing from a request without it or with one it cannot read, is withheld without the evidence it names, parses both shapes, preserves a vendor-specific `atype`, skips entries with nothing to offer, and refuses invalid base64, invalid JSON and an oversized value.
- A template key cookie under the public prefix, and `sharedId`, are refused, and either may be listed for bypass.
