# Design Spec: OpenRTB is an auction protocol module, and core's auction names no protocol

**Status:** Proposed, 2026-10-14. Implemented on a working branch of the fork, with no pull request raised yet.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-03-19-auction-orchestration-flow-design.md`
**Related PRs:** none yet
**Issues:** IABTechLab/trusted-server#1336, one of the removals listed in IABTechLab/trusted-server#777
**Last updated:** 2026-10-14

> **Why this spec exists.** Core built every demand source's request as an OpenRTB 2.6 bid request and read every answer as an OpenRTB bid response, so a demand source on another protocol could not be an auction module without translating to OpenRTB first, and a change in what OpenRTB carries was a change in core.

## 1. Problem

The demand seam made each demand source a module and left them all speaking the one protocol core knew. Core held the OpenRTB request builder, the response reader, the extension objects, the hook trait a source filled in, which named OpenRTB fields (`imp.tagid`, `site.ref`, `regs`), and the generated OpenRTB types as a dependency of its own. The identifiers a request carries were typed as OpenRTB's `Eid`, though the identity cookie, the `x-ts-eids` header and the identify API carry them with no auction involved.

A standard is a vendor as much as a product is, and the auction protocol was the largest standard core still carried.

## 2. Design

**Core keeps a neutral demand seam.** A compiled demand source implements `CompiledDemand` in `crates/trusted-server-core/src/auction/demand.rs`, which has two jobs. `build_request` takes the routed input (the slots and their formats, the publisher, the permission state the auction resolved and the identifiers the request may carry), the transport facts (the admitted headers, the attested client address and the Do Not Track fact) and the signing input core settled, and answers an `OutboundRequest`, being the body, the headers, the ids of the slots the request asks for and any state the source keeps for reading the answer, or `None` when the source has nothing to ask for. `parse_response` reads the answer into core's bids. Core sends what the source built, with `POST`, to the endpoint the plan settled, and reads the answer against the slots the request asked for.

**Admission stays in core.** Whatever protocol a source reads, a bid is admitted when it names a slot the request carried and its dimensions match exactly one format that slot asked for. Those rules, the dimension index, the rejection reasons and the bounded diagnostics are `crates/trusted-server-core/src/auction/admission.rs`, so a reader in any crate admits a bid by the rules core applies. The notification policy and the count of bidder parameters a source ignored are the provider's, in `provider.rs`.

**The identifiers are the identity module's.** `Eid` and `Uid` are `crates/trusted-server-core/src/ec/eids.rs`, beside the helpers that resolve them from the identity graph and carry them in the response header. The identity cookie, that header, the identify API and the auction request body carry this shape, and a protocol formats it into its own request.

**OpenRTB is a protocol module.** `crates/auction-protocol/openrtb` holds the standard. `wire.rs` re-exports the generated types and adds the extension objects (`user.ext`, `regs.ext`, `ext.trusted_server`). `request.rs` builds the bid request from the routed facts, every standard field chosen through the source's field policy, and signs it. `response.rs` reads an ordinary bid response. `demand.rs` holds what a source that speaks OpenRTB decides, the `OpenRtbDemand` trait with the field policy, the extensions, the outbound headers and the response reader, and `OpenRtbDriver`, which runs one such source through core's seam. A demand implementation that speaks OpenRTB depends on this crate, implements `OpenRtbDemand`, and its `compile` wraps the source in `OpenRtbDriver::new`. Core's `as_any` answers with the wrapped source, so an implementation reads its own settings back as before. The plain implementation, `auction-protocol.openrtb`, lives in the same crate, as it did.

**The demand crates depend on the protocol crate.** `auction.prebid-server` and `auction.aps` implement `OpenRtbDemand` and take the wire types from the protocol crate. `ad-server.mock` reads bids by core's admission rules and names no protocol crate.

**Core no longer holds the driver.** `auction/openrtb.rs`, its tests, and core's `openrtb.rs` are gone, and `auction/demand.rs` carries none of the OpenRTB field policy. Core's own test stand-ins speak a plain JSON of their own, so core's tests show what a source is given and what core does with its answer without naming a protocol.

**A deployment's settings do not change.** Every `[demand.<name>]` table, every implementation name and every setting in a table reads as before.

## 3. What is not in this change

- The response the auction endpoint returns to the caller of `POST /auction`. `auction/formats.rs` still writes it as an OpenRTB bid response through the generated types, with the orchestrator's summary in `ext.orchestrator` and a typed renderer in `ext.trusted_server`, and the endpoint still calls that conversion, so core still depends on `trusted-server-openrtb` for it. Moving it needs a protocol trait the endpoint calls and a rule for which protocol writes the response when the selected sources' protocols differ, or when none is selected and the auction is disabled, which today still answers an empty OpenRTB response. That rule is a decision for the maintainers before the move.
- The settings keys that name the standard, `openrtb_atype` in a partner's table and `openrtb_only` as a consent forwarding mode, which a deployment's document writes and which are unchanged.
- The Prebid EID cookie parser and the consent decoders, which move in series of their own.

## 4. Tests

- Core's provider sends exactly one request with the method, headers and body the source built, keeps a source's completion of a bare host, and makes no request for a source with nothing to ask for.
- Core's admission helpers admit an exact match, infer a unique match, refuse an ambiguous or mismatched one and a slot the request did not carry, read a dimension as a positive whole number or absent, and count rejections by reason.
- The protocol crate's driver holds the consent matrix, the language limits and the device attributes under the plain implementation and under a stand-in that departs from it, omits an impression a source leaves without demand and the whole request when none is left, refuses a source that claims `ext.trusted_server`, signs after the source's extensions with every owned key, formats the identifiers as the user's `eids`, and hands core the headers the field policy asks for and the slot ids the request carries.
- The protocol crate's reader isolates a malformed sibling, accepts omitted and `USD` currency and refuses another, admits bids by core's rules, keeps the exact returned seat for the notification policy, and classifies `204`, a non-success status, a redirect and unreadable JSON.
- Every demand crate's own tests pass unchanged in what they assert, reading the request the driver builds through the protocol crate's test helper.
