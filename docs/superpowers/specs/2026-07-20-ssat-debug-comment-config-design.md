# SSAT Debug Comment Configuration Design

**Date:** 2026-07-20

**Status:** Proposed

**Issue:** [IABTechLab/trusted-server#935](https://github.com/IABTechLab/trusted-server/issues/935) — "For SSAT, make debug comment configurable"

## Summary

The server-side auction template (SSAT) can inject a `<!-- ts-debug: ... -->`
HTML comment before the bids `<script>`, gated by `[debug]
auction_html_comment = true`. Today its content is entirely fixed: a hardcoded
metadata allowlist, a hardcoded 512-byte creative preview, and no way to omit
sections. Operators debugging "why did we get bids for some slots and not
others" need to see the actual request sent to and response received from each
provider — including HTTP status codes and upstream error text — without
recompiling or patching the redaction logic.

This design adds a config table, `[debug.auction_html_comment_options]`,
alongside the existing bool, with:

1. Section toggles (provider responses / mediator response / bids array).
2. A configurable subset of an expanded, still-hardcoded metadata allowlist.
3. A `verbosity` switch (`redacted` default, `full` opt-in) that bypasses the
   allowlist and creative truncation entirely for deep debugging.

## Goals

1. Let an operator omit sections of the dump to keep it small/focused.
2. Let an operator select which of the already-safe metadata keys to surface.
3. Surface `http_status` and `upstream_message` — already captured
   server-side, currently absent from the allowlist by omission, not
   design — so "was it a 400, and for what reason" is answerable in the
   default (redacted) mode.
4. Provide an explicit, loudly-documented `full` mode for the rare case where
   an operator needs the raw per-bidder request/response (PBS `debug.httpcalls`)
   to diagnose a specific auction, accepting the PII exposure that implies.
5. Never let configuration weaken the fail-closed guarantee in redacted mode:
   identity-bearing data (device IP, geo, `user.ext.eids`, TC consent string)
   must be unreachable via `metadata_keys` regardless of what an operator
   configures.

## Non-goals

- Per-provider filtering (dump only named SSPs). Explicitly declined — out of
  scope for this change.
- Configurable size limits. Explicitly declined — `MAX_BID_CREATIVE_DUMP_BYTES`
  (512) and `MAX_AUCTION_DEBUG_DUMP_BYTES` (256KB) stay hardcoded constants;
  neither becomes operator-tunable. Note this does **not** mean both apply in
  both verbosity modes: `MAX_AUCTION_DEBUG_DUMP_BYTES` (the 256KB total cap)
  is unconditional in both `Redacted` and `Full`, but `MAX_BID_CREATIVE_DUMP_BYTES`
  (the 512-byte per-bid preview) is `Redacted`-only by design — `Full` skips
  creative truncation entirely (see Rendering / Data Flow). "Hardcoded" means
  "not config-driven," not "applied unconditionally in every mode."
- Adding failure-reason instrumentation to provider adapters that don't
  capture any today. Notably, `AuctionResponse::no_bid()`
  ([auction/types.rs:296](../../../crates/trusted-server-core/src/auction/types.rs#L296))
  always carries empty metadata, and `aps.rs`
  ([integrations/aps.rs:546](../../../crates/trusted-server-core/src/integrations/aps.rs#L546))
  never attaches a no-fill reason. `verbosity=full` will show nothing more for
  an APS no-bid than redacted mode does today, because there is nothing more
  captured server-side. Fixing that is separate, larger, per-provider work —
  tracked as a follow-up issue, not part of this spec.
- A bare-bool-or-table union deserializer for backward compatibility. The
  existing `auction_html_comment` bool is untouched; the new table is a
  sibling field, so no migration path is needed.
- Changing `inject_adm_for_testing` (the existing "raw `adm` into
  `window.tsjs.bids`" debug flag). Independent toggle, different channel
  (client JS state vs. HTML comment), no interaction with this design.
- Tightening `Bid`-level fields (`Bid.metadata`, `nurl`, `burl`) to a
  fail-closed allowlist. These already pass through `redact_bid_for_dump`
  unfiltered today in both verbosity modes
  ([publisher.rs:966-972](../../../crates/trusted-server-core/src/publisher.rs#L966-L972)),
  pre-existing and tracked separately as issue #925. Unaffected by, and
  orthogonal to, this design.

## Current Behavior (today, for reference)

`prepend_auction_debug_comment`
([publisher.rs:950](../../../crates/trusted-server-core/src/publisher.rs#L950))
unconditionally:

- Includes `provider_responses` and `mediator_response` (when present).
- Includes every provider's `bids` array.
- Filters each response's metadata to a fixed 7-key allowlist:
  `error_type, status, message, responsetimemillis, errors, warnings, bidstatus`
  ([publisher.rs:878](../../../crates/trusted-server-core/src/publisher.rs#L878)).
- Previews each bid's `creative` to 512 bytes
  ([publisher.rs:893](../../../crates/trusted-server-core/src/publisher.rs#L893)).
- Neutralizes `-->`/`--!>` comment terminators and caps the total serialized
  dump at 256KB, unconditionally.

Notably absent from the allowlist today despite already being captured
server-side by the prebid integration:

- `http_status` — numeric HTTP status from a non-2xx PBS response
  ([prebid.rs:2025](../../../crates/trusted-server-core/src/integrations/prebid.rs#L2025)).
- `upstream_message` / `upstream_message_truncated` — the actual PBS error
  body text, only populated when `[integrations.prebid] debug = true`
  ([prebid.rs:2038-2046](../../../crates/trusted-server-core/src/integrations/prebid.rs#L2038-L2046)).

Also captured server-side, but deliberately excluded from the allowlist and
staying that way in redacted mode: the raw `debug` subtree
(`httpcalls`/`resolvedrequest`) that PBS returns when `integrations.prebid.debug`
is on ([prebid.rs:1945-1948](../../../crates/trusted-server-core/src/integrations/prebid.rs#L1945-L1948)).
This carries the resolved OpenRTB request: device IP, geo, `user.ext.eids`, TC
consent string, per-bidder request/response bodies. This is the data `full`
verbosity exists to expose, on explicit opt-in.

## Config Schema

```rust
/// Debug-only features. All flags default to `false` (off in production).
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DebugConfig {
    // ...existing fields unchanged (ja4_endpoint_enabled, auction_html_comment,
    // inject_adm_for_testing)...

    /// Behavior of the ts-debug comment. Only consulted when
    /// `auction_html_comment` is true. Defaults reproduce today's fixed
    /// output plus the two allowlist additions below.
    #[serde(default)]
    pub auction_html_comment_options: AuctionDebugCommentOptions,
}

/// Behavior of the `<!-- ts-debug: ... -->` auction dump.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuctionDebugCommentOptions {
    /// Include the `provider_responses` section at all.
    #[serde(default = "default_true")]
    pub include_provider_responses: bool,

    /// Include `mediator_response` when a mediator ran.
    #[serde(default = "default_true")]
    pub include_mediator_response: bool,

    /// Include each provider's `bids` array (vs. status/metadata only).
    #[serde(default = "default_true")]
    pub include_bids: bool,

    /// Subset of `AUCTION_DEBUG_METADATA_ALLOWLIST` to surface in `Redacted`
    /// mode. Keys outside the fixed allowlist are always dropped, regardless
    /// of what is listed here — this is a subset selector, not a new
    /// allowlist. Ignored entirely when `verbosity = Full`.
    #[serde(default = "default_auction_debug_metadata_keys")]
    pub metadata_keys: Vec<String>,

    /// `Redacted` (default): `metadata_keys` subset only, creative preview
    /// truncated to `MAX_BID_CREATIVE_DUMP_BYTES`.
    /// `Full`: raw `response.metadata` verbatim, including the `debug`
    /// subtree (httpcalls/resolvedrequest — device IP, geo, eids, TC consent
    /// string — when `integrations.prebid.debug` is also on), and no
    /// creative truncation. The 256KB total dump cap and comment-terminator
    /// neutralization still apply.
    ///
    /// NEVER enable `Full` in production: identity-bearing request/response
    /// data becomes visible to any visitor via view-source.
    #[serde(default)]
    pub verbosity: AuctionDebugCommentVerbosity,
}

impl Default for AuctionDebugCommentOptions {
    fn default() -> Self {
        Self {
            include_provider_responses: true,
            include_mediator_response: true,
            include_bids: true,
            metadata_keys: default_auction_debug_metadata_keys(),
            verbosity: AuctionDebugCommentVerbosity::Redacted,
        }
    }
}

impl AuctionDebugCommentOptions {
    fn normalize(&mut self) {
        self.metadata_keys = self
            .metadata_keys
            .drain(..)
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
            .collect();
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuctionDebugCommentVerbosity {
    #[default]
    Redacted,
    Full,
}
```

`DebugConfig` derives `#[derive(Default, ...)]`
([settings.rs:1895](../../../crates/trusted-server-core/src/settings.rs#L1895)),
which requires every field's type to implement `Default`. The hand-written
`impl Default for AuctionDebugCommentOptions` above is required for this to
compile — a naive `#[derive(Default)]` would produce
`include_provider_responses: false` etc., contradicting the serde field
defaults (`true`/`true`/`true`/full-allowlist/`Redacted`). Both `Default` and
serde's per-field `#[serde(default = "...")]` must agree.

`AUCTION_DEBUG_METADATA_ALLOWLIST` moves from `publisher.rs` into `settings.rs`
as the single canonical superset (publisher.rs imports it; it's the same list
used both as `default_auction_debug_metadata_keys()`'s return value and as the
fail-closed intersection filter — see Security Invariants). New expanded list
drops the old allowlist's `"status"` key (verified: no production code path
writes `response.metadata["status"]` — only an unrelated `telemetry.rs` test
does — so this is an intentional cleanup of a key nothing ever populates, not
an accidental narrowing):

```rust
const AUCTION_DEBUG_METADATA_ALLOWLIST: &[&str] = &[
    "error_type",
    "http_status",
    "message",
    "upstream_message",
    "upstream_message_truncated",
    "responsetimemillis",
    "errors",
    "warnings",
    "bidstatus",
];
```

`Settings::finalize_deserialized`
([settings.rs:2038-2059](../../../crates/trusted-server-core/src/settings.rs#L2038-L2059))
— the associated fn that already calls `settings.integrations.normalize();
settings.proxy.normalize(); settings.image_optimizer.normalize();` on a
locally-owned `settings: Self` — gains
`settings.debug.auction_html_comment_options.normalize();` alongside those.
(Not `Settings::normalize()`: no such method touches `DebugConfig` today, and
the lines this draft originally cited — settings.rs:1388-1406 — are
`ProxyAssetRoute::normalize()`, an unrelated per-route proxy struct with no
`debug` field.) `DebugConfig` is not touched by any normalize/prepare_runtime
pipeline today; this is the first field on it that needs one.

## Rendering / Data Flow

`prepend_auction_debug_comment` gains an `options: &AuctionDebugCommentOptions`
parameter:

```rust
let mut dump = serde_json::Map::new();
if options.include_provider_responses {
    dump.insert(
        "provider_responses".to_string(),
        Value::Array(
            result.provider_responses.iter()
                .map(|r| redact_response_for_dump(r, options))
                .collect(),
        ),
    );
}
if options.include_mediator_response
    && let Some(mediator_response) = &result.mediator_response
{
    dump.insert("mediator_response".to_string(), redact_response_for_dump(mediator_response, options));
}
```

`redact_response_for_dump(response, options)`:

- `Redacted`: `metadata` = `response.metadata` filtered to
  `options.metadata_keys ∩ AUCTION_DEBUG_METADATA_ALLOWLIST`. The intersection
  is computed here, at the render call — this is the actual security
  boundary, not the config struct itself.
- `Full`: `metadata` = `response.metadata.clone()`, unfiltered.
- `bids` = `[]` when `!options.include_bids`; otherwise each bid goes through
  `redact_bid_for_dump(bid, options)`.

`redact_bid_for_dump(bid, options)`:

- `Redacted`: `creative` truncated to `MAX_BID_CREATIVE_DUMP_BYTES` (512),
  as today.
- `Full`: `creative` passed through untruncated.

Unconditional regardless of `options` (safety nets, not redaction controls):

- comment-terminator neutralization (`-->` → `-- >`, `--!>` → `-- !>`)
- `MAX_AUCTION_DEBUG_DUMP_BYTES` (256KB) final cap on the serialized dump

**Wiring** — production call site
[publisher.rs:1394](../../../crates/trusted-server-core/src/publisher.rs#L1394)
(the two existing test call sites for `prepend_auction_debug_comment`, around
publisher.rs:2638 and 2699, also need the new parameter — same signature
change ripples to both):

```rust
if settings.debug.auction_html_comment {
    prepend_auction_debug_comment(
        "stream",
        &result,
        ad_bids_state,
        &settings.debug.auction_html_comment_options,
    );
}
```

## Security Invariants

1. **`metadata_keys` is a subset selector, never a new allowlist.** The
   `AUCTION_DEBUG_METADATA_ALLOWLIST` superset is a hardcoded Rust const, not
   config-driven. An operator listing `"debug"` (or any other key outside the
   superset) in `metadata_keys` has zero effect in `Redacted` mode — the
   intersection silently drops it. This must hold even when the operator's
   intent is clearly to widen access; fail-closed means the config cannot
   widen the boundary, only narrow what's already inside it.
2. **`Full` verbosity is the only path to identity-bearing data**, and it
   requires two independent, explicit opt-ins to have any effect for prebid:
   `debug.auction_html_comment_options.verbosity = "full"` AND
   `integrations.prebid.debug = true` (the latter is what makes PBS return the
   `debug.httpcalls` subtree at all). Neither flag alone exposes anything new.
3. **Comment-terminator neutralization and the total byte cap are
   unconditional** — they are HTML-injection and page-bloat safety nets, not
   privacy controls, and must never be gated behind `verbosity` or any other
   option.
4. **Bad `verbosity` values fail config load**, not silently fall back to
   `Redacted`. An unrecognized string is a serde deserialize error at startup
   — loud failure over silent (mis)interpretation.

## Edge Cases and Behavior Changes

- **`metadata_keys = []`**: valid; yields `metadata: {}` per response. An
  operator can explicitly request zero metadata while still seeing bids/status.
- **`verbosity=Full` value is provider-dependent**: only `prebid.rs` populates
  `metadata["debug"]` today. For `aps` and other direct integrations, `Full`
  only removes creative truncation and the metadata filter — there's no
  `debug` subtree to reveal because none is captured. See Non-goals.
- **Truncation lands mid-structure more often under `Full`**: the final-string
  truncation in `render_dump` is byte-boundary-safe (UTF-8) but not
  JSON-structure-aware — pre-existing behavior, unchanged. `Full` mode's raw
  httpcalls payloads are much larger than redacted previews, so hitting the
  256KB cap (and getting a truncated, non-parseable tail) becomes routine
  rather than exceptional. No new per-provider cap is being added (see
  Non-goals) — this is a known, accepted tradeoff.
- **Default output changes for existing operators**: anyone already running
  `auction_html_comment = true` gets 3 additional keys
  (`http_status`, `upstream_message`, `upstream_message_truncated`) in the
  default dump with zero config changes on their part. Not a security
  regression — still fail-closed, still no identity data — but it is a
  default-output change worth calling out explicitly rather than smuggling in
  silently.

## Testing Strategy

All in the existing `publisher.rs` test module (plain `#[test]` fns,
Arrange-Act-Assert, matching existing `auction_debug_comment_*` tests — no
`rstest` in this file today):

- `default_options_reproduce_current_behavior` — regression: default struct
  vs. today's hardcoded output, identical except: the unused `status` key
  (never written by any production path) is gone, and the 3 new keys
  (`http_status`, `upstream_message`, `upstream_message_truncated`) are added.
- `metadata_keys_empty_yields_empty_metadata_object`
- `metadata_keys_attack_vector_debug_key_never_surfaces_in_redacted_mode` —
  configuring `metadata_keys = ["debug"]` under `Redacted` still produces no
  `debug` key. This is the load-bearing security test for this whole design.
- `verbosity_full_includes_raw_debug_subtree_when_present`
- `verbosity_full_skips_creative_truncation`
- `verbosity_full_still_hits_overall_byte_cap`
- `verbosity_full_still_neutralises_comment_terminators` — extends the
  existing `auction_debug_comment_neutralises_every_comment_terminator_vector`
  test to run under `Full` too, proving neutralization isn't bypassable via
  verbosity.
- `include_provider_responses_false_omits_section_entirely`
- `include_mediator_response_false_omits_even_when_mediator_ran`
- `include_bids_false_yields_empty_bids_array_not_omitted_response`
- `normalize_trims_and_drops_empty_metadata_keys`
- `bad_verbosity_string_fails_config_load`

## Example TOML

```toml
[debug]
# NEVER enable in production. Injects a redacted per-provider auction dump
# before </body>. See [debug.auction_html_comment_options] for content control.
auction_html_comment = false

[debug.auction_html_comment_options]
include_provider_responses = true
include_mediator_response = true
include_bids = true
metadata_keys = [
    "error_type", "http_status", "message",
    "upstream_message", "upstream_message_truncated",
    "responsetimemillis", "errors", "warnings", "bidstatus",
]
# "redacted" (default) or "full". NEVER "full" in production — exposes
# device IP, geo, consent string, and eids via view-source when
# integrations.prebid.debug is also enabled.
verbosity = "redacted"
```

## Follow-up / Out of Scope

- File a separate issue: add no-bid/failure reason capture to `aps.rs` and
  audit other direct-integration providers (kargo, ttd, etc.) for the same
  gap. Until that lands, `verbosity=full` cannot explain a no-bid from those
  providers — there is nothing more recorded server-side to show.
